//! MCP stdio transport for the existing Herdr control socket.
//! The host injects its authoritative browser command catalog and validation
//! callback. This crate never exposes approval grants, and never starts Herdr.
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::Duration,
};
const MCP_PROTOCOL: &str = "2025-06-18";
const MAX_LINE: usize = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct ToolArg {
    pub name: String,
    pub required: bool,
}
#[derive(Clone, Debug)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub args: Vec<ToolArg>,
}
fn offered<'a>(tools: &'a [ToolDefinition]) -> impl Iterator<Item=&'a ToolDefinition> {
    tools.iter().filter(|t| t.name.starts_with("browser.") && t.name != "browser.approve")
}
fn rpc_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}
fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
fn value_type(name: &str) -> &'static str {
    match name {
        "target" | "condition" => "object",
        "x" | "y" | "delta_x" | "delta_y" => "number",
        "timeout_ms" | "wait_ms" | "max_nodes" => "integer",
        "focus" => "boolean",
        _ => "string",
    }
}
pub fn tools_list(commands: &[ToolDefinition]) -> Value {
    json!({"tools": offered(commands).map(|command| {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::<&str>::new();
        for arg in &command.args {
            properties.insert(arg.name.clone(), json!({"type":value_type(&arg.name)}));
            if arg.required { required.push(&arg.name); }
        }
        json!({
            "name":command.name,
            "description":command.description,
            "inputSchema":{
                "type":"object",
                "properties":properties,
                "required":required,
                "additionalProperties":false
            }
        })
    }).collect::<Vec<_>>()})
}

/// Send a validated request to the existing Herdr App control socket.
/// Caller enforces host command argument validation first.
pub fn forward_socket(socket_path: &Path, name: &str, args: &Value) -> Result<Value, String> {
    if !name.starts_with("browser.") || name == "browser.approve" {
        return Err("Unknown browser tool".into());
    }
    let mut socket = UnixStream::connect(socket_path)
        .map_err(|e| format!("Herdr must be open to use browser tools: {e}"))?;
    socket.set_read_timeout(Some(Duration::from_secs(10))).map_err(|e|e.to_string())?;
    let request = json!({"do":name,"args":args});
    let data = serde_json::to_vec(&request).map_err(|e|e.to_string())?;
    if data.len() > MAX_LINE { return Err("browser input exceeds 1MiB".into()); }
    socket.write_all(&data).and_then(|_|socket.write_all(b"\n")).map_err(|e|e.to_string())?;
    let mut reply = String::new();
    let read = BufReader::new(&socket).read_line(&mut reply).map_err(|e|e.to_string())?;
    if read==0 { return Err("Herdr control socket closed without a result".into()); }
    if reply.len()>MAX_LINE { return Err("Herdr control response exceeds 1MiB".into()); }
    serde_json::from_str(&reply).map_err(|e|format!("invalid Herdr response: {e}"))
}
fn tool_response(result: Result<Value, String>) -> Value {
    match result {
        Ok(result) => {
            let is_error = result.get("error").is_some()
                || result.get("status").and_then(Value::as_str)==Some("failed");
            let structured = if result.is_object() {result.clone()} else {json!({"value":result})};
            json!({
                "content":[{"type":"text","text":result.to_string()}],
                "structuredContent":structured,
                "isError":is_error
            })
        }
        Err(error) => json!({
            "content":[{"type":"text","text":error}],
            "isError":true
        })
    }
}
pub fn process(
    request: &Value,
    tools: &[ToolDefinition],
    app_version: &str,
    forward: &impl Fn(&str, &Value)->Result<Value, String>,
) -> Option<Value> {
    let id = request.get("id")?.clone();
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return Some(rpc_error(id,-32600,"invalid JSON-RPC request: method must be a string"));
    };
    let params = &request["params"];
    let result = match method {
        "initialize" => json!({
            "protocolVersion":MCP_PROTOCOL,
            "capabilities":{"tools":{"listChanged":false}},
            "serverInfo":{
                "name":"herdr-webview",
                "title":"Herdr WebView",
                "version":app_version,
            }
        }),
        "ping" => json!({}),
        "tools/list" => tools_list(tools),
        "prompts/list" => json!({"prompts":[]}),
        "resources/list" => json!({"resources":[]}),
        "tools/call" => {
            let Some(name)=params.get("name").and_then(Value::as_str) else {
                return Some(rpc_error(id,-32602,"tools/call requires the tool name"));
            };
            if !offered(tools).any(|t| t.name==name) {
                return Some(rpc_error(id,-32602,"unknown browser tool"));
            }
            let args=params.get("arguments").cloned().unwrap_or_else(||json!({}));
            if !args.is_object(){
                return Some(rpc_error(id,-32602,"tools/call arguments must be an object"));
            }
            tool_response(forward(name,&args))
        }
        _ => return Some(rpc_error(id,-32601,"method not supported")),
    };
    Some(rpc_result(id,result))
}
pub fn serve_stdio(
    tools: &[ToolDefinition],
    app_version: &str,
    forward: impl Fn(&str, &Value)->Result<Value,String>,
) -> i32 {
    let stdin=io::stdin();
    let stdout=io::stdout();
    let mut output=stdout.lock();
    for line in stdin.lock().lines() {
        let line=match line {
            Ok(line) => line,
            Err(error) => {eprintln!("browser MCP stdin failed: {error}");return 1;}
        };
        if line.len()>MAX_LINE {
            eprintln!("browser MCP rejected oversized request");
            return 1;
        }
        let request=match serde_json::from_str::<Value>(&line) {
            Ok(request) => request,
            Err(error) => {
                let answer=rpc_error(Value::Null,-32700,&format!("parse error: {error}"));
                if writeln!(output,"{answer}").is_err(){return 1;}
                continue;
            }
        };
        if let Some(response)=process(&request,tools,app_version,&forward) {
            if writeln!(output,"{response}").is_err()||output.flush().is_err(){return 1;}
        }
    }
    0
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixtures() -> Vec<ToolDefinition> {
        vec![ToolDefinition{
            name:"browser.click".into(), description:"Click element".into(),
            args:vec![
                ToolArg{name:"pane".into(), required:true},
                ToolArg{name:"target".into(), required:true},
            ],
        }, ToolDefinition{name:"browser.approve".into(),description:"Forbidden".into(),args:vec![]}]
    }
    #[test]
    fn mcp_schema_and_no_self_approval() {
        let list=tools_list(&fixtures());
        let tools=list["tools"].as_array().unwrap();
        assert_eq!(tools.len(),1);
        assert_eq!(tools[0]["name"],"browser.click");
        assert_eq!(tools[0]["inputSchema"]["properties"]["target"]["type"],"object");
        assert_eq!(tools[0]["inputSchema"]["required"],json!(["pane","target"]));
        let blocked=process(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser.approve"}}),
            &fixtures(),"app-test", &|_,_|panic!("host approval may not be forwarded")).unwrap();
        assert_eq!(blocked["error"]["code"],-32602);
    }
    #[test]
    fn initialize_and_unknown_method() {
        let init=process(&json!({"jsonrpc":"2.0","id":9,"method":"initialize"}),&fixtures(),"0.1.16-webview.1",&|_,_|Ok(Value::Null)).unwrap();
        assert_eq!(init["result"]["serverInfo"]["version"],"0.1.16-webview.1");
        let bad=process(&json!({"jsonrpc":"2.0","id":9,"method":"invalid"}),&fixtures(),"v",&|_,_|Ok(Value::Null)).unwrap();
        assert_eq!(bad["error"]["code"],-32601);
        assert!(process(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}),&fixtures(),"v",&|_,_|Ok(Value::Null)).is_none());
    }
}
