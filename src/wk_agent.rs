//! WKWebView Agent observation and explicit page-context eval.
//! Only a native Wry callback can produce a completed result; a fire-and-
//! forget JavaScript call is never reported as success.
use crate::browser_api::{activating_key, locator, wait_condition, wait_probe};
use serde_json::{json, Value};

const MAX_SCRIPT: usize = 65_536;
const MAX_RESULT: usize = 512 * 1024;

/// Build one bounded, pane-local JS request. Destructive/opaque page scripts
/// always require approval through Herdr's native GPUI action dialog.
pub fn script(command: &str, args: &Value, approved: bool) -> Result<String, String> {
    let body = match command {
        "browser.snapshot" => {
            let max = args.get("max_nodes").and_then(Value::as_u64).unwrap_or(100).clamp(1, 400);
            format!(r#"
const nodes=[...document.querySelectorAll('a,button,input,textarea,select,[role],[tabindex],h1,h2,h3')];
const elements=[];
for(const el of nodes){{ if(elements.length>={max})break;if(!visible(el))continue;elements.push(summary(el)); }}
return {{ok:true,result:{{snapshot_id:String(performance.timeOrigin)+'|'+location.href,
document_id:String(performance.timeOrigin)+'|'+location.href,title:document.title,
url:location.href,elements,truncated:nodes.length>elements.length}}}};
"#)
        }
        "browser.query" | "browser.inspect" => {
            locator(args, false)?;
            let limit = if command == "browser.inspect" {1} else {50};
            format!("const els=select();return {{ok:true,result:{{count:els.length,truncated:els.length>{limit},elements:els.slice(0,{limit}).map(summary)}}}};")
        }
        "browser.click" => {
            locator(args, false)?;
            let expected=args.get("document_id").unwrap_or(&Value::Null);
            if !expected.is_null() && expected.as_str().is_none_or(|s| s.len()>2048){
                return Err("INVALID_ARGUMENT: document_id must be a string".into());
            }
            let expected_json=expected.to_string();
            format!(r#"
const current=String(performance.timeOrigin)+'|'+location.href;
const expected={expected_json};
if(expected!==null&&expected!==current)return {{ok:false,error:'STALE_DOCUMENT: target document changed'}};
const els=select();
if(els.length!==1)return {{ok:false,error:els.length===0?'LOCATOR_NOT_FOUND':'LOCATOR_AMBIGUOUS'}};
const el=els[0];
if(!visible(el)||el.disabled||el.getAttribute('aria-disabled')==='true')
 return {{ok:false,error:'ELEMENT_NOT_ACTIONABLE'}};
el.scrollIntoView({{block:'center',inline:'center',behavior:'instant'}});
const box=el.getBoundingClientRect();
const x=box.x+box.width/2,y=box.y+box.height/2;
const top=document.elementFromPoint(x,y);
if(!top||(top!==el&&!el.contains(top)))return {{ok:false,error:'ELEMENT_OBSCURED'}};
const risk=riskOf(el);
if(!{approved}&&(risk.dangerous||risk.unknown))
 return {{ok:false,error:'APPROVAL_REQUIRED: native click on uncertain or consequential site control '+risk.label}};
return {{ok:true,result:{{x,y,native:true,document_id:current,label:risk.label}}}};
"#)
        }
        "browser.click_at" => {
            if !approved {
                return Err("APPROVAL_REQUIRED: native coordinate click may activate an unknown site control".into());
            }
            let x=args.get("x").and_then(Value::as_f64)
                .ok_or("INVALID_ARGUMENT: click_at x must be numeric")?;
            let y=args.get("y").and_then(Value::as_f64)
                .ok_or("INVALID_ARGUMENT: click_at y must be numeric")?;
            if !x.is_finite()||!y.is_finite()||x<0.0||y<0.0||x>8192.0||y>8192.0 {
                return Err("INVALID_ARGUMENT: click_at is outside supported viewport bounds".into());
            }
            format!("return {{ok:true,result:{{x:{x},y:{y},native:true}}}};")
        }
        "browser.fill" | "browser.type" => {
            locator(args, true)?;
            let key=if command=="browser.fill"{"value"}else{"text"};
            let value=args.get(key).and_then(Value::as_str)
                .ok_or("INVALID_ARGUMENT: missing text value")?;
            if value.is_empty() || value.chars().count()>128
               || !value.chars().all(|c| c.is_ascii_alphanumeric() || c==' ') {
                return Err("UNSUPPORTED_TEXT: WK native text supports 1..128 ASCII letters, digits, spaces".into());
            }
            let select=if command=="browser.fill" {
                "if(typeof el.select==='function')el.select();else if(el.isContentEditable)document.getSelection().selectAllChildren(el);"
            } else { "" };
            format!(r#"
const els=select();if(els.length!==1)return {{ok:false,error:els.length===0?'LOCATOR_NOT_FOUND':'LOCATOR_AMBIGUOUS'}};
const el=els[0];const gate=editable(el);if(gate)return {{ok:false,error:gate.error}};
if(sensitive(el)&&!{approved})
 return {{ok:false,error:'APPROVAL_REQUIRED: typing sensitive form values'}};
el.focus();{select}
return {{ok:true,result:{{native:true,text_ready:true}}}};
"#)
        }
        "browser.press" => {
            let key=args.get("key").and_then(Value::as_str)
                .ok_or("INVALID_ARGUMENT: native WK key is required")?;
            if !["Enter","Return","Escape","Tab","Backspace","Delete",
                  "ArrowLeft","ArrowRight","ArrowDown","ArrowUp","Space"].contains(&key) {
                return Err("INVALID_ARGUMENT: unsupported native WK key".into());
            }
            if activating_key(key) && !approved {
                return Err("APPROVAL_REQUIRED: native key could submit or activate a site action".into());
            }
            let focus = if args.get("target").is_some() {
                locator(args, true)?;
                format!(r#"
const matches=select();
if(matches.length!==1)return {{ok:false,error:matches.length===0?'LOCATOR_NOT_FOUND':'LOCATOR_AMBIGUOUS'}};
if(!visible(matches[0]))return {{ok:false,error:'ELEMENT_NOT_ACTIONABLE'}};
matches[0].focus();
"#)
            } else { String::new() };
            let key=serde_json::to_string(key).map_err(|e|e.to_string())?;
            format!("{focus}return {{ok:true,result:{{key:{key},native:true}}}};")
        }
        "browser.scroll" => {
            // Numbers, not page-controlled expressions. Use the real viewport
            // center when coordinates are omitted; native AppKit validates
            // the final location against the attached WK child bounds.
            let pixel = |key:&str, default:f64| -> Result<f64,String> {
                let value=args.get(key).map(Value::as_f64).unwrap_or(Some(default))
                    .ok_or_else(||format!("INVALID_ARGUMENT: {key} requires a number"))?;
                if !value.is_finite()||value.abs()>8192.0 {
                    return Err(format!("INVALID_ARGUMENT: {key} is out of range"));
                }
                Ok(value)
            };
            let dx=pixel("delta_x",0.0)?;
            let dy=pixel("delta_y",0.0)?;
            if dx.abs()>4096.0||dy.abs()>4096.0||(dx.round()==0.0&&dy.round()==0.0) {
                return Err("INVALID_ARGUMENT: WK scroll requires a bounded nonzero delta".into());
            }
            let x=if args.get("x").is_some(){pixel("x",0.0)?.to_string()}else{"innerWidth/2".into()};
            let y=if args.get("y").is_some(){pixel("y",0.0)?.to_string()}else{"innerHeight/2".into()};
            format!("return {{ok:true,result:{{x:{x},y:{y},delta_x:{dx},delta_y:{dy},native:true}}}};")
        }
        "browser.wait" => {
            let cond=wait_condition(args)?;
            let probe=wait_probe(&cond);
            format!(r#"{probe}
return {{ok:true,result:{{matched:!!matched,url:location.href}}}};"#)
        }
        "browser.eval" => {
            if !approved {
                return Err("APPROVAL_REQUIRED: arbitrary JavaScript can bypass website action permissions".into());
            }
            let expression = args.get("expression").and_then(Value::as_str)
                .ok_or("INVALID_ARGUMENT: expression requires a string")?;
            if expression.len()>MAX_SCRIPT {
                return Err("INVALID_ARGUMENT: expression exceeds 64KiB".into());
            }
            // JSON-encoded string prevents the script from changing our wrapper.
            let serialized = serde_json::to_string(expression).map_err(|e| e.to_string())?;
            format!("return {{ok:true,result:(0,eval)({serialized})}};")
        }
        _ => return Err("UNSUPPORTED_ENGINE: WKWebView native input/screenshots are not yet implemented".into()),
    };
    let target = if matches!(command, "browser.query" | "browser.inspect" | "browser.click" | "browser.press" | "browser.fill" | "browser.type") {
        args["target"].clone()
    } else { Value::Null };
    Ok(format!(r#"(() => {{
try {{
const t={target};
{helpers}
{body}
}} catch (e) {{
return {{ok:false,error:String(e&&e.message||e)}};
}}
}})()"#, target=target,helpers=crate::browser_api::DOM_HELPERS,body=body))
}

/// Wry serializes evaluate_script_with_callback results as JSON. Treat
/// missing or malformed callback payloads as errors, never success.
pub fn parse_callback(raw: &str) -> Result<Value, String> {
    if raw.len()>MAX_RESULT { return Err("RESULT_TOO_LARGE: WKWebView output exceeds 512KiB".into()); }
    let response: Value = serde_json::from_str(raw)
        .map_err(|e|format!("WK_CALLBACK_INVALID: {e}"))?;
    if response.get("ok").and_then(Value::as_bool) != Some(true) {
        let message = response.get("error").and_then(Value::as_str).unwrap_or("no result");
        if message.starts_with("APPROVAL_REQUIRED:") {
            return Err(message.to_owned());
        }
        return Err(format!("WK_EVALUATION_FAILED: {message}"));
    }
    let result = response.get("result").cloned().unwrap_or(Value::Null);
    if serde_json::to_vec(&result).map_err(|e| e.to_string())?.len()>MAX_RESULT {
        return Err("RESULT_TOO_LARGE: WKWebView result exceeds 512KiB".into());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wk_observation_commands_are_bounded_and_pane_local() {
        let snapshot=script("browser.snapshot", &json!({"max_nodes":4000}), false).unwrap();
        assert!(snapshot.contains("elements.length>=400"));
        let query=script("browser.query", &json!({"target":{"role":"button","name":"Search"}}),false).unwrap();
        assert!(query.contains("const t="));
        assert!(query.contains("els.slice(0,50)"));
        assert!(script("browser.query", &json!({"target":{"css":"button","text":"Search"}}),false).is_err());
        assert!(script("browser.query", &json!({"target":{"css":"button","x":12}}),false).is_err());
        assert!(script("browser.click", &json!({"target":{"css":"button"}}),true).unwrap().contains("native:true"));
    }
    #[test]
    fn wk_native_scroll_uses_bounded_pixel_deltas_and_viewport_coordinates(){
        let generated=script("browser.scroll",&json!({"delta_y":200.0}),false).unwrap();
        assert!(generated.contains("innerWidth/2"));
        assert!(generated.contains("innerHeight/2"));
        assert!(generated.contains("delta_y:200"));
        assert!(generated.contains("native:true"));
        for args in [
            json!({}),json!({"delta_y":0}),json!({"delta_y":9000}),
            json!({"delta_y":true}),json!({"delta_y":200,"x":-50_000}),
            json!({"delta_y":200,"y":"document.cookie"})
        ] {
            assert!(script("browser.scroll",&args,false).is_err(),"unsafe WK scroll {args}");
        }
    }
    #[test]
    fn wk_wait_supports_typed_conditions_without_injecting_js_literals() {
        let generated=script("browser.wait",&json!({
            "condition":{"selector":"#submit","state":"visible","text":"Done","url":"/next"},
            "timeout_ms":1500
        }),false).unwrap();
        assert!(generated.contains("document.querySelector(c.selector)"));
        assert!(generated.contains("matched:!!matched"));
        assert!(generated.contains("\"selector\":\"#submit\""));
        assert!(script("browser.wait",&json!({"condition":{"state":"hidden"}}),false).is_err());
        assert!(script("browser.wait",&json!({"condition":{"selector":"input","state":"bogus"}}),false).is_err());
        assert!(script("browser.wait",&json!({"condition":{"selector":"input"},"timeout_ms":30001}),false).is_err());
        assert!(script("browser.wait",&json!({"condition":{"selector":"input","exec":"alert(1)"}}),false).is_err());
    }
    #[test]
    fn wk_native_clicks_require_locators_and_native_approval() {
        let target=json!({"target":{"css":"button"}});
        let source=script("browser.click",&target,false).unwrap();
        assert!(source.contains("elementFromPoint"));
        assert!(source.contains("APPROVAL_REQUIRED: native click"));
        assert!(source.contains("STALE_DOCUMENT"));
        assert!(script("browser.click",&json!({"target":{"css":"button","label":"x"}}),false).is_err());
        assert!(script("browser.click",&json!({"target":{"css":"button"},"document_id":17}),false).is_err());
        assert!(script("browser.click_at",&json!({"x":20.0,"y":30.0}),false)
            .unwrap_err().starts_with("APPROVAL_REQUIRED:"));
        assert!(script("browser.click_at",&json!({"x":20.0,"y":30.0}),true)
            .unwrap().contains("native:true"));
        assert!(script("browser.click_at",&json!({"x":-1,"y":30}),true).is_err());
        assert!(parse_callback(r#"{"ok":false,"error":"APPROVAL_REQUIRED: confirm deletion"}"#)
            .unwrap_err().starts_with("APPROVAL_REQUIRED:"));
    }
    #[test]
    fn wk_uses_shared_sensitive_input_and_destructive_destination_policy() {
        let click=script("browser.click",&json!({"target":{"css":"a"}}),false).unwrap();
        assert!(click.contains("riskOf(el)"));
        assert_eq!(click.matches("const riskOf=").count(),1,"the shared risk policy is embedded exactly once");
        assert_eq!(click.matches("const riskyHref=").count(),1,"click risk policy must not be copied per engine");
        let fill=script("browser.fill",&json!({"target":{"css":"#otp"},"value":"481902"}),false).unwrap();
        assert!(fill.contains("sensitive(el)"));
        assert!(fill.contains("one-time-code"),"must include shared OTP redaction policy");
    }

    #[test]
    fn native_text_entry_has_bounded_locator_and_never_evaluates_input_as_js() {
        let fill=script("browser.fill",&json!({"target":{"css":"#field"},"value":"Hello World"}),false).unwrap();
        assert!(fill.contains("el.select()"));
        assert!(fill.contains("APPROVAL_REQUIRED: typing sensitive"));
        assert!(!fill.contains("Hello World"), "native text must stay outside the page JS bridge");
        assert!(script("browser.type",&json!({"target":{"css":"#field"},"text":"Ada"}),false)
            .unwrap().contains("text_ready:true"));
        for v in ["hello\nworld","中","!"] {
            assert!(script("browser.fill",&json!({"target":{"css":"input"},"value":v}),false).is_err());
        }
        assert!(script("browser.fill",&json!({"target":{"role":"textbox"},"value":"Hello"}),false).is_err());
    }
    #[test]
    fn native_keypress_is_allowlisted_and_submission_requires_approval() {
        assert!(script("browser.press", &json!({"key":"Tab"}), false).unwrap().contains("native:true"));
        for key in ["Enter","Return","Space"] {
            assert!(script("browser.press", &json!({"key":key}), false).unwrap_err()
                .starts_with("APPROVAL_REQUIRED:"),"{key} must pass the shared activation gate");
            assert!(script("browser.press", &json!({"key":key}), true).unwrap().contains("native:true"));
        }
        assert!(script("browser.press", &json!({"key":"a"}), true).is_err());
        assert!(script("browser.press", &json!({"key":"Tab","target":{"css":"#field"}}), false)
            .unwrap().contains("matches[0].focus()"));
        assert!(script("browser.press", &json!({"key":"Tab","target":{"text":"unsafe"}}),false).is_err());
    }
    #[test]
    fn arbitrary_eval_requires_the_native_approval_flow() {
        let args=json!({"expression":"document.title"});
        assert!(script("browser.eval",&args,false).unwrap_err().starts_with("APPROVAL_REQUIRED:"));
        assert!(script("browser.eval",&args,true).unwrap().contains("(0,eval)"));
        assert!(script("browser.eval",&json!({"expression":"x".repeat(70000)}),true).is_err());
    }
    #[test]
    fn wk_callback_must_have_verified_json_and_bounded_result() {
        assert_eq!(parse_callback(r#"{"ok":true,"result":{"count":2}}"#).unwrap(),json!({"count":2}));
        assert!(parse_callback("null").is_err());
        assert!(parse_callback("undefined").is_err());
        assert!(parse_callback(r#"{"ok":false,"error":"bad selector"}"#).unwrap_err().contains("bad selector"));
        assert!(parse_callback(&format!(r#"{{"ok":true,"result":"{}"}}"#,"x".repeat(600000))).is_err());
    }
}
