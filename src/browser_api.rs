//! Shared browser page policy (DOM helpers, locators, wait semantics and the
//! native-approval gates) plus the bounded Chrome CDP routines that execute it.
//! WKWebView's wk_agent scripts embed the same helpers, so both engines judge
//! risk, sensitivity and liveness by one set of rules. Called only from the
//! host's verified Herdr-pane dispatcher, never from a web page script.
use crate::chrome_cdp::Session;
use serde_json::{Value, json};
use std::{sync::{Arc, atomic::{AtomicBool,AtomicU64,Ordering}},time::{Duration,Instant},fs,io::Write,path::Path,os::unix::fs::{PermissionsExt,OpenOptionsExt}};
static SCREENSHOT_SEQ: AtomicU64 = AtomicU64::new(1);
const MAX_TEXT: usize = 65536;

fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    let value = args.get(key).and_then(Value::as_str).ok_or_else(|| format!("INVALID_ARGUMENT: {key} requires a string"))?;
    if value.len() > MAX_TEXT {return Err(format!("INVALID_ARGUMENT: {key} exceeds size limit"))}
    Ok(value)
}
fn coordinate(args: &Value, key: &str, default: f64) -> Result<f64, String> {
    let n = args.get(key).map(Value::as_f64).unwrap_or(Some(default))
        .ok_or_else(|| format!("INVALID_ARGUMENT: {key} requires a number"))?;
    if !n.is_finite() || n.abs()>100000.0 {return Err(format!("INVALID_ARGUMENT: invalid {key}"))}
    Ok(n)
}
/// One locator schema for both engines and every command: exactly one of
/// css/role/label/text/test_id/ref, plus optional name/exact/snapshot_id.
/// `css_only` is for native text/key targets that focus a single element.
pub(crate) fn locator<'a>(args: &'a Value, css_only: bool) -> Result<&'a Value,String> {
    let v=args.get("target").ok_or("INVALID_ARGUMENT: target required")?;
    let obj=v.as_object().ok_or("INVALID_ARGUMENT: target must be an object")?;
    if css_only && (obj.len()!=1 || !obj.contains_key("css")) {
        return Err("INVALID_ARGUMENT: native text/key target supports one CSS selector".into());
    }
    let types=["css","role","label","text","test_id","ref"];
    if types.iter().filter(|key|obj.contains_key(**key)).count()!=1 ||
        obj.keys().any(|k|!types.contains(&k.as_str()) && !["name","exact","snapshot_id"].contains(&k.as_str())) {
        return Err("INVALID_ARGUMENT: target requires exactly one locator type".into());
    }
    for (key,val) in obj {
        if key=="exact" {if !val.is_boolean(){return Err("INVALID_ARGUMENT: target.exact must be boolean".into())}}
        else if !val.is_string() || val.as_str().is_some_and(|s|s.is_empty()||s.len()>512){
            return Err(format!("INVALID_ARGUMENT: target.{key} must be a non-empty string of at most 512 bytes"));
        }
    }
    Ok(v)
}

/// Keys that can submit a form or activate a focused control under either
/// engine's naming (Return aliases Enter, Space activates buttons). Every
/// engine gates these on native approval through this one list.
pub(crate) fn activating_key(key: &str) -> bool {
    matches!(key, "Enter" | "Return" | "Space")
}

/// Shared `browser.wait` condition contract for both engines: same keys, same
/// bounds, same timeout, so one engine cannot accept what the other rejects.
pub(crate) fn wait_condition(args: &Value) -> Result<Value, String> {
    let condition=args.get("condition").and_then(Value::as_object)
        .ok_or("INVALID_ARGUMENT: wait condition must be an object")?;
    if condition.is_empty() || condition.len()>4
        || condition.keys().any(|key| !["url","selector","state","text"].contains(&key.as_str())) {
        return Err("INVALID_ARGUMENT: unsupported or empty wait condition".into());
    }
    for (key, value) in condition {
        let text=value.as_str().ok_or("INVALID_ARGUMENT: wait fields must be strings")?;
        let limit=if key=="url"{2048}else{512};
        if text.is_empty()||text.len()>limit {
            return Err(format!("INVALID_ARGUMENT: wait condition.{key} is too long or empty"));
        }
    }
    if condition.contains_key("state") &&
        (!condition.contains_key("selector") ||
         !["visible","hidden"].contains(&condition["state"].as_str().unwrap_or("")))
    {
        return Err("INVALID_ARGUMENT: state requires selector and visible|hidden".into());
    }
    if !condition.contains_key("url") && !condition.contains_key("selector") && !condition.contains_key("text") {
        return Err("INVALID_ARGUMENT: wait needs a URL, selector or text condition".into());
    }
    if args.get("timeout_ms").is_some_and(|v| v.as_u64().is_none_or(|t| t==0 || t>30000)) {
        return Err("INVALID_ARGUMENT: timeout_ms must be 1..30000".into());
    }
    Ok(Value::Object(condition.clone()))
}

/// Shared wait probe: both engines must agree on what visible/hidden means.
/// Leaves `matched` and `url` for the caller's result envelope.
pub(crate) fn wait_probe(cond: &Value) -> String {
    format!(r#"const c={cond};let matched=true;
if(c.url!==undefined)matched=matched&&location.href.includes(c.url);
if(c.selector!==undefined){{
 const el=document.querySelector(c.selector);
 const shown=!!el&&visible(el);
 matched=matched&&(c.state==='hidden'?!shown:shown);
}}
if(c.text!==undefined)matched=matched&&!!document.body&&document.body.innerText.includes(c.text);"#,cond=cond)
}

pub(crate) const DOM_HELPERS: &str = r#"
const value=t;
const visible=e=>{
  const b=e.getBoundingClientRect(),s=getComputedStyle(e);
  return b.width>0&&b.height>0&&s.display!=='none'&&s.visibility!=='hidden';
};
const simple=x=>String(x==null?'':x).trim().slice(0,240);
const role=e=>e.getAttribute('role')||({A:'link',BUTTON:'button',TEXTAREA:'textbox',INPUT:'textbox',SELECT:'combobox'}[e.tagName]||'');
const name=e=>simple(e.getAttribute('aria-label')||e.labels&&e.labels[0]&&e.labels[0].textContent||
   e.getAttribute('alt')||e.getAttribute('placeholder')||e.textContent);
const path=e=>{
  const p=[];let cur=e;
  for(let i=0;cur&&cur.nodeType===1&&i<15;i++,cur=cur.parentElement) {
    const peers=cur.parentElement?[...cur.parentElement.children].filter(x=>x.tagName===cur.tagName):[cur];
    p.unshift(cur.tagName.toLowerCase()+':nth-of-type('+(peers.indexOf(cur)+1)+')');
  }
  return p.join(' > ');
};
const sensitive=e=>{
  const kind=String(e.getAttribute('type')||'').toLowerCase();
  const autocomplete=String(e.getAttribute('autocomplete')||'').toLowerCase();
  const identity=[e.id,e.getAttribute('name'),e.getAttribute('aria-label')].filter(Boolean).join(' ');
  return kind==='password'||kind==='hidden'
    ||/(?:^|\s)(?:current-password|new-password|one-time-code|cc-[\w-]+)(?:\s|$)/i.test(autocomplete)
    ||/(?:password|passwd|passcode|pin|otp|totp|2fa|cvv|cvc|credit.?card|card.?number|secret|access.?token|api.?key|security.?code)/i.test(identity);
};
const editable=e=>{
  if(!visible(e)||e.disabled||e.readOnly)return {error:'ELEMENT_NOT_ACTIONABLE'};
  if(!['INPUT','TEXTAREA'].includes(e.tagName)&&!e.isContentEditable)return {error:'NOT_EDITABLE'};
  return null;
};
// A harmless-looking label does not make a submit button or a destructive
// destination safe. This is a conservative preflight shared by both engines;
// native approval is still required before clicking unknown controls.
const riskOf=e=>{
  const label=name(e).toLowerCase();
  const form=e.closest('form');
  const submits=(e.tagName==='BUTTON'||e.tagName==='INPUT')&&e.type==='submit'
    &&(!form||form.getAttribute('role')!=='search');
  const href=e.tagName==='A'?e.getAttribute('href')||'':'';
  const riskyHref=/(?:^|[/?#&=_-])(pay|purchase|checkout|delete|remove|transfer|confirm|order|submit|unsubscribe|logout)(?:[/?#&=_-]|$)/i.test(href);
  const dangerous=/pay|buy|purchase|checkout|send|delete|remove|transfer|publish|confirm|order|submit|unsubscribe|account|save|security/i.test(label)
    ||submits||riskyHref;
  const known=(e.tagName==='A'&&/^https?:/.test(e.href))||
   /^(search|next|back|previous|open|close|show|hide|expand|collapse|filter|play|pause)$/i.test(label);
  return {label:label.slice(0,120),dangerous,unknown:!known};
};
const summary=e=>{
  const b=e.getBoundingClientRect();
  return {ref:path(e),role:role(e),name:sensitive(e)?'[REDACTED]':name(e),
    tag:e.tagName.toLowerCase(),text:sensitive(e)?'[REDACTED]':simple(e.textContent),
    visible:visible(e),enabled:!e.disabled&&e.getAttribute('aria-disabled')!=='true',
    bounds:{x:b.x,y:b.y,width:b.width,height:b.height},
    value:sensitive(e)?'[REDACTED]':
      (e.tagName==='INPUT'||e.tagName==='TEXTAREA'?simple(e.value):null)};
};
const equals=(a,b)=>value.exact===false?String(a).toLowerCase().includes(String(b).toLowerCase()):String(a)===String(b);
const select=()=>{
  if(value.css)return [...document.querySelectorAll(value.css)];
  if(value.ref){
    if(value.snapshot_id&&value.snapshot_id!==String(performance.timeOrigin)+'|'+location.href)
      throw Error('STALE_SNAPSHOT');
    return [...document.querySelectorAll(value.ref)];
  }
  return [...document.querySelectorAll('a,button,input,textarea,select,label,[role],[tabindex],[contenteditable]')].slice(0,6000).filter(e=>{
    if(value.test_id)return equals(e.getAttribute('data-testid'),value.test_id);
    if(value.role)return role(e)===value.role&&(!value.name||equals(name(e),value.name));
    if(value.label)return equals(name(e),value.label);
    if(value.text)return equals(simple(e.textContent),value.text);
    return false;
  });
};
"#;
fn js(loc:&Value,body:&str)->String {
    format!("(() => {{ const t = {}; {} {} }})()",loc,DOM_HELPERS,body)
}
fn unique(session:&Session,loc:&Value,body:&str)->Result<Value,String>{
    let script=js(loc,&format!("const els=select(); if(els.length!==1)return {{error:els.length===0?'LOCATOR_NOT_FOUND':'LOCATOR_AMBIGUOUS',count:els.length}};const el=els[0];{body}"));
    let value=session.eval(&script)?;
    if let Some(error)=value["error"].as_str(){return Err(format!("{error}: {} matches",value["count"]))}
    Ok(value)
}
// Share the same no-symlink, owner-only storage policy as native WebKit.
// A pre-existing browser-screenshots symlink must never redirect Agent PNGs
// into another directory, even if that directory is writable.
fn ensure_private_screenshot_dir(dir:&Path)->Result<(),String>{
    fs::create_dir_all(dir).map_err(|e|format!("SCREENSHOT_WRITE_FAILED: {e}"))?;
    let meta=fs::symlink_metadata(dir).map_err(|e|format!("SCREENSHOT_WRITE_FAILED: {e}"))?;
    if !meta.is_dir() || meta.file_type().is_symlink(){
        return Err("SCREENSHOT_WRITE_FAILED: screenshot directory must not be a symlink".into());
    }
    fs::set_permissions(dir,fs::Permissions::from_mode(0o700))
        .map_err(|e|format!("SCREENSHOT_WRITE_FAILED: {e}"))
}
fn capped(value:Value)->Result<Value,String>{
    if serde_json::to_vec(&value).map_err(|e|e.to_string())?.len()>524288{
        return Err("RESULT_TOO_LARGE: response exceeds 512KiB".into())
    }
    Ok(value)
}
pub fn run(session:&Arc<Session>,command:&str,args:&Value,approved:bool,cancelled:&AtomicBool)->Result<Value,String>{
    if cancelled.load(Ordering::Acquire) {
        return Err("CANCELLED: browser operation stopped before execution".into());
    }
    let result=match command {
        "browser.snapshot"=>{
            let max=args.get("max_nodes").and_then(Value::as_u64).unwrap_or(100).clamp(1,400);
            // Snapshot, query and inspect MUST share the same bounded,
            // sensitive-field-aware DOM serializer. A separate snapshot
            // mapper previously bypassed the redaction used by inspect.
            let expression=format!(r#"(() => {{
const t=null;
{DOM_HELPERS}
const nodes=[...document.querySelectorAll('a,button,input,textarea,select,[role],[tabindex],h1,h2,h3')];
const items=[];
for(const el of nodes){{
 if(items.length>={max})break;
 if(!visible(el))continue;
 items.push(summary(el));
}}
return {{snapshot_id:String(performance.timeOrigin)+'|'+location.href,
document_id:String(performance.timeOrigin)+'|'+location.href,
title:document.title,url:location.href,elements:items,truncated:nodes.length>items.length}};
}})()"#);
            session.eval(&expression)?
        }
        "browser.query"|"browser.inspect"=>{
            let loc=locator(args,false)?;
            let limit=if command=="browser.inspect"{1}else{50};
            session.eval(&js(loc,&format!("const els=select();return {{count:els.length,truncated:els.length>{limit},elements:els.slice(0,{limit}).map(summary)}};")))?
        }
        "browser.click"=>{
            let loc=locator(args,false)?;
            let info=unique(session,loc,r#"
if(!visible(el)||el.disabled||el.getAttribute('aria-disabled')==='true')return {error:'ELEMENT_NOT_ACTIONABLE'};
el.scrollIntoView({block:'center',inline:'center',behavior:'instant'});
const b=el.getBoundingClientRect(),x=b.x+b.width/2,y=b.y+b.height/2;
const top=document.elementFromPoint(x,y);
if(!top||(top!==el&&!el.contains(top)))return {error:'ELEMENT_OBSCURED'};
const risk=riskOf(el);
return {x,y,dangerous:risk.dangerous,unknown:risk.unknown,label:risk.label};"#)?;
            if !approved&&(info["dangerous"]==true||info["unknown"]==true){
                return Err(format!("APPROVAL_REQUIRED: click on site control {:?}",info["label"]));
            }
            if cancelled.load(Ordering::Acquire){return Err("CANCELLED".into())}
            session.click_at(info["x"].as_f64().ok_or("invalid X")?,info["y"].as_f64().ok_or("invalid Y")?)?;
            json!({"clicked":true})
        }
        "browser.click_at"=>{
            if !approved{return Err("APPROVAL_REQUIRED: coordinate click may activate an unknown control".into())}
            let x=coordinate(args,"x",f64::NAN)?;
            let y=coordinate(args,"y",f64::NAN)?;
            if x<0.0||y<0.0||x>8192.0||y>8192.0{return Err("INVALID_ARGUMENT: coordinates out of bounds".into())}
            if cancelled.load(Ordering::Acquire){return Err("CANCELLED".into())}
            session.click_at(x,y)?;
            json!({"clicked":true})
        }
        "browser.fill"|"browser.type"=>{
            let loc=locator(args,false)?;
            let value=required(args,if command=="browser.fill"{"value"}else{"text"})?;
            // The DOM bridge only locates/focuses/selects; actual typing is
            // dispatched through Chrome Input.insertText, not el.value=...
            let expression=if command=="browser.fill" {r#"
const gate=editable(el);if(gate)return gate;
el.focus();
if(typeof el.select==='function')el.select();
else if(el.isContentEditable)document.getSelection().selectAllChildren(el);
return {ready:true,sensitive:sensitive(el)};
"#}else{r#"
const gate=editable(el);if(gate)return gate;
el.focus();return {ready:true,sensitive:sensitive(el)};"#};
            let info=unique(session,loc,expression)?;
            if info["sensitive"]==true && !approved {return Err("APPROVAL_REQUIRED: entering sensitive credentials/payment data".into())}
            if cancelled.load(Ordering::Acquire){return Err("CANCELLED".into())}
            session.insert_text(value)?;
            json!({"typed":true,"length":value.chars().count()})
        }
        "browser.press"=>{
            let key=required(args,"key")?;
            if activating_key(key) && !approved {
                return Err("APPROVAL_REQUIRED: native key could submit or activate a site action".into());
            }
            if let Some(loc)=args.get("target"){unique(session,loc,"el.focus();return {focused:true};")?;}
            let mapped=match key {
                "Enter"|"Return"=>"Enter","Escape"=>"Escape","Tab"=>"Tab",
                "Backspace"=>"Backspace","Delete"=>"Delete",
                "ArrowLeft"=>"ArrowLeft","ArrowRight"=>"ArrowRight",
                "ArrowUp"=>"ArrowUp","ArrowDown"=>"ArrowDown","Space"=>" ",
                _=>return Err("INVALID_ARGUMENT: unsupported key; use browser.type for text".into()),
            };
            if cancelled.load(Ordering::Acquire){return Err("CANCELLED".into())}
            session.command("Input.dispatchKeyEvent",json!({"type":"rawKeyDown","key":mapped}))?;
            session.command("Input.dispatchKeyEvent",json!({"type":"keyUp","key":mapped}))?;
            json!({"pressed":mapped})
        }
        "browser.scroll"=>{
            let x=coordinate(args,"x",400.0)?;
            let y=coordinate(args,"y",300.0)?;
            let dx=coordinate(args,"delta_x",0.0)?;
            let dy=coordinate(args,"delta_y",0.0)?;
            if cancelled.load(Ordering::Acquire){return Err("CANCELLED".into())}
            session.scroll(x,y,dx,dy)?;
            json!({"scrolled":true})
        }
        "browser.wait"=>{
            let cond=wait_condition(args)?;
            let probe=wait_probe(&cond);
            let js=format!(r#"(() => {{
const t=null;
{DOM_HELPERS}
{probe}
return {{matched:!!matched,url:location.href}};}})()"#);
            let timeout=Duration::from_millis(args["timeout_ms"].as_u64().unwrap_or(5000).min(30000));
            let start=Instant::now();
            loop {
                if cancelled.load(Ordering::Acquire){return Err("CANCELLED".into())}
                let result=session.eval(&js)?;
                if result["matched"]==true{break result}
                if start.elapsed()>=timeout{return Err("TIMEOUT: wait condition not satisfied".into())}
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        "browser.eval"=>{
            if !approved{return Err("APPROVAL_REQUIRED: arbitrary JavaScript can bypass website action permissions".into())}
            if cancelled.load(Ordering::Acquire){return Err("CANCELLED".into())}
            session.eval(required(args,"expression")?)?
        }
        "browser.screenshot"=>{
            let bytes=session.screenshot()?;
            if bytes.len()>12*1024*1024{return Err("RESULT_TOO_LARGE: PNG exceeds 12MiB".into())}
            let img=image::load_from_memory_with_format(&bytes,image::ImageFormat::Png)
                .map_err(|e|format!("SCREENSHOT_INVALID: {e}"))?;
            let dir=crate::app_home::app_dir().join("browser-screenshots");
            ensure_private_screenshot_dir(&dir)?;
            let file=dir.join(format!("chrome-{}-{}.png",std::process::id(),SCREENSHOT_SEQ.fetch_add(1,Ordering::Relaxed)));
            let mut f=fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&file).map_err(|e|format!("SCREENSHOT_WRITE_FAILED: {e}"))?;
            f.write_all(&bytes).map_err(|e|e.to_string())?;
            json!({"path":file.to_string_lossy(),"width":img.width(),"height":img.height(),"format":"png"})
        }
        _=>return Err("UNSUPPORTED_ACTION: browser operation not implemented".into()),
    };
    capped(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancelled_request_has_no_write_permission(){
        let cancel=AtomicBool::new(true);
        assert!(cancel.load(Ordering::Acquire));
    }
    #[test]
    fn locator_must_be_unique_in_schema(){
        assert!(locator(&json!({"target":{"css":"button"}}),false).is_ok());
        assert!(locator(&json!({"target":{"role":"button","name":"Search"}}),false).is_ok());
        assert!(locator(&json!({"target":{"css":"button","text":"Search"}}),false).is_err());
        assert!(locator(&json!({"target":{"x":1,"y":2}}),false).is_err());
        assert!(locator(&json!({"target":{"css":""}}),false).is_err());
        assert!(locator(&json!({"target":{"css":"#f"}}),true).is_ok());
        assert!(locator(&json!({"target":{"role":"textbox"}}),true).is_err());
    }
    #[test]
    fn one_shared_policy_gates_activating_keys_and_click_risk(){
        for key in ["Enter","Return","Space"] {
            assert!(activating_key(key),"{key} must require native approval");
        }
        for key in ["Tab","Escape","Backspace","ArrowUp"] {
            assert!(!activating_key(key));
        }
        // The risky-destination and submit rules live exactly once, in the
        // shared helpers both engines embed; per-engine copies may drift.
        assert!(DOM_HELPERS.contains("const riskOf="));
        assert!(DOM_HELPERS.contains("const editable="));
        assert_eq!(DOM_HELPERS.matches("const riskyHref=").count(),1);
        assert_eq!(DOM_HELPERS.matches("const sensitive=").count(),1);
    }
    #[test]
    fn wait_conditions_share_one_contract(){
        assert!(wait_condition(&json!({"condition":{"text":"Done"}})).is_ok());
        assert!(wait_condition(&json!({"condition":{"selector":"#a","state":"hidden"}})).is_ok());
        for bad in [json!({"condition":{}}),json!({"condition":{"state":"hidden"}}),
                    json!({"condition":{"selector":"#a","state":"bogus"}}),
                    json!({"condition":{"selector":"input","exec":"alert(1)"}}),
                    json!({"condition":{"text":"t"},"timeout_ms":30001})] {
            assert!(wait_condition(&bad).is_err(),"unsafe wait condition {bad}");
        }
        let probe=wait_probe(&json!({"selector":"#submit","state":"visible"}));
        assert!(probe.contains("const c="));
        assert!(probe.contains("\"selector\":\"#submit\""));
        assert!(probe.contains("\"state\":\"visible\""));
        assert!(probe.contains("visible(el)"),"both engines must share visible() semantics");
    }
    #[test]
    fn chrome_screenshot_directory_rejects_symlink_redirection(){
        use std::os::unix::fs::symlink;
        let parent=std::env::temp_dir().join(format!("herdr-chrome-png-security-{}",std::process::id()));
        let _=fs::remove_dir_all(&parent);
        fs::create_dir_all(&parent).unwrap();
        let real=parent.join("real");
        assert!(ensure_private_screenshot_dir(&real).is_ok());
        assert_eq!(fs::metadata(&real).unwrap().permissions().mode()&0o777,0o700);
        let alias=parent.join("browser-screenshots");
        symlink(&real,&alias).unwrap();
        assert!(ensure_private_screenshot_dir(&alias).unwrap_err().contains("symlink"));
        fs::remove_dir_all(parent).unwrap();
    }
    #[test]
    fn chrome_snapshot_and_inspect_share_sensitive_redaction(){
        let source=include_str!("browser_api.rs");
        assert!(source.contains("{DOM_HELPERS}"),"snapshot must embed shared DOM helpers");
        assert!(source.contains("items.push(summary(el))"),"snapshot must use redacting summary");
        assert!(DOM_HELPERS.contains("name:sensitive(e)?'[REDACTED]'"));
        assert!(DOM_HELPERS.contains("text:sensitive(e)?'[REDACTED]'"));
    }

    #[test]
    fn shared_dom_helper_redacts_passwords_otp_and_card_fields(){
        for marker in ["current-password","one-time-code","cc-","api.?key","[REDACTED]"] {
            assert!(DOM_HELPERS.contains(marker),"missing sensitive redaction rule: {marker}");
        }
    }

    #[test]
    fn browser_operation_output_must_be_bounded(){
        assert!(capped(json!({"data":"x".repeat(600000)})).is_err());
        assert!(coordinate(&json!({"x":"abc"}),"x",0.0).is_err());
        assert!(required(&json!({"name":"hello"}),"name").is_ok());
    }
}
