// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

const HANG: Duration = Duration::from_secs(60);
const NAV: &str = "https://mail.proton.me/u/0/inbox";
const API_URL: &str = "https://mail.proton.me/api/mail/v4/messages";
const KEY_URL: &str = "https://mail.proton.me/api/auth/v4/sessions/local/key";

struct Page {
    cdp: browser::CdpClient,
    child: browser::BrowserChild,
    session: String,
    _profile: ProfileLock,
    _root: aterm_tempfile::TempDir,
}

impl Page {
    fn new() -> Self {
        let home = aterm_uds::host::tool_home();
        let pin = aterm_uds::host::chrome::host_pin().expect("native browser platform pin");
        let path = browser::browser_cache_roots(home.as_deref())
            .into_iter()
            .map(|root| {
                aterm_uds::host::chrome::pinned_binary(
                    &aterm_uds::host::chrome::pinned_dir(&root, pin.platform),
                    pin.platform,
                )
            })
            .find(|path| path.is_file())
            .expect("run targo --unverified verify provision for the pinned headless browser");
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("child-home");
        let profile = root.path().join("profile");
        aterm_types::fs_restricted::ensure_private_dir(&home).unwrap();
        aterm_types::fs_restricted::ensure_private_dir(&profile).unwrap();
        let lock = ProfileLock::claim(&profile).unwrap();
        let executable = browser::HeadlessBrowser {
            path,
            is_headless_shell: true,
        };
        let (command_read, command_write) = std::io::pipe().unwrap();
        let (event_read, event_write) = std::io::pipe().unwrap();
        let command = browser::headless_browser_command(&executable, &profile, &|key| match key {
            "HOME" => Some(home.as_os_str().to_os_string()),
            "XDG_CONFIG_HOME" | "XDG_CACHE_HOME" | "XDG_DATA_HOME" | "XDG_RUNTIME_DIR" => {
                Some(home.join(key).into_os_string())
            }
            _ => std::env::var_os(key),
        });
        let child = browser::BrowserChild::new(
            aterm_uds::spawnfd::spawn_with_two_fds(
                command,
                OwnedFd::from(command_read),
                OwnedFd::from(event_write),
            )
            .unwrap(),
        );
        let mut cdp = browser::CdpClient::new(command_write, event_read);
        let version = cdp
            .call(None, "Browser.getVersion", json!({}), HANG)
            .unwrap();
        let expected = format!("HeadlessChrome/{}", aterm_uds::host::chrome::PIN_VERSION);
        assert_eq!(version["product"].as_str(), Some(expected.as_str()));
        let target = cdp
            .call(
                None,
                "Target.createTarget",
                json!({"url":"about:blank"}),
                HANG,
            )
            .unwrap();
        let attached = cdp
            .call(
                None,
                "Target.attachToTarget",
                json!({"targetId":target["targetId"].clone(),"flatten":true}),
                HANG,
            )
            .unwrap();
        let session = attached["sessionId"].as_str().unwrap().to_owned();
        let mut page = Self {
            cdp,
            child,
            session,
            _profile: lock,
            _root: root,
        };
        page.call("Page.enable", json!({}));
        page.call("Runtime.enable", json!({}));
        page.call("Fetch.enable", json!({"patterns":[{"urlPattern":"https://mail.proton.me/*","requestStage":"Request"}]}));
        page.cdp
            .take_events("Page.loadEventFired", Some(&page.session));
        page.call("Runtime.evaluate", json!({"expression":format!("setTimeout(()=>location.href={},0); 'scheduled'", aterm_json::to_string(NAV).unwrap()),"returnByValue":true}));
        let request = page.paused(NAV);
        page.fulfill(&request, "text/html", b"<!doctype html><title>owned@example.invalid</title><link rel=icon href='data:,'><button data-testid='sidebar:compose'>Compose</button>").unwrap();
        page.event("Page.loadEventFired");
        page
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        self.cdp
            .call(Some(&self.session), method, params, HANG)
            .unwrap()
    }

    fn event(&mut self, method: &str) -> Value {
        let until = Instant::now() + HANG;
        loop {
            let events = self.cdp.take_events(method, Some(&self.session));
            if let Some(event) = events.into_iter().next() {
                return event["params"].clone();
            }
            assert!(
                Instant::now() < until,
                "native browser event {method} did not arrive"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn paused(&mut self, url: &str) -> Value {
        let request = self.event("Fetch.requestPaused");
        assert_eq!(request["request"]["url"].as_str(), Some(url));
        assert_eq!(request["request"]["method"].as_str(), Some("GET"));
        request
    }

    fn fulfill(&mut self, request: &Value, kind: &str, body: &[u8]) -> Result<Value, &'static str> {
        self.cdp.call(Some(&self.session), "Fetch.fulfillRequest", json!({"requestId":request["requestId"].clone(),"responseCode":200,"responseHeaders":[{"name":"Content-Type","value":kind}],"body":aterm_codec::base64::encode(body).unwrap()}), HANG)
    }

    fn evaluate(&mut self, expression: &str) -> Value {
        let response = self.call(
            "Runtime.evaluate",
            json!({"expression":expression,"awaitPromise":true,"returnByValue":true}),
        );
        assert!(
            response.get("exceptionDetails").is_none(),
            "native fixture setup: {response:?}"
        );
        response["result"]["value"].clone()
    }

    fn start(&mut self, script: &str) -> String {
        let script = format!(
            "({script}).then(value=>({{ok:true,value}}),error=>({{ok:false,name:String(error.name),message:String(error.message)}}))"
        );
        let response = self.call(
            "Runtime.evaluate",
            json!({"expression":script,"awaitPromise":false,"returnByValue":false}),
        );
        response["result"]["objectId"]
            .as_str()
            .expect("native script promise")
            .to_owned()
    }

    fn outcome(&mut self, promise: &str) -> Value {
        let response = self.call(
            "Runtime.awaitPromise",
            json!({"promiseObjectId":promise,"returnByValue":true}),
        );
        assert!(
            response.get("exceptionDetails").is_none(),
            "native promise adapter: {response:?}"
        );
        response["result"]["value"].clone()
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        let _ = self
            .cdp
            .call(None, "Browser.close", json!({}), Duration::from_secs(2));
        self.child.close().unwrap();
    }
}

fn install_session(page: &mut Page) -> Value {
    page.evaluate(r#"(async()=>{
      const raw=new Uint8Array(32).fill(7),iv=new Uint8Array(16).fill(5);
      const key=await crypto.subtle.importKey('raw',raw,'AES-GCM',false,['encrypt']);
      const clear=new TextEncoder().encode(JSON.stringify({keyPassword:'owned-mail-key'}));
      const encrypted=new Uint8Array(await crypto.subtle.encrypt({name:'AES-GCM',iv,additionalData:new TextEncoder().encode('session')},key,clear));
      const blob=new Uint8Array(iv.length+encrypted.length);blob.set(iv);blob.set(encrypted,iv.length);
      const b64=value=>btoa(String.fromCharCode(...value));
      localStorage.setItem('ps-0',JSON.stringify({UID:'owned-uid',blob:b64(blob),payloadVersion:2}));
      return b64(raw);
    })()"#)
}

fn response(
    page: &mut Page,
    key: &Value,
    kind: u8,
    age: u64,
    end: Option<Instant>,
) -> Result<Value, String> {
    let wait = page_request_remaining(end, Instant::now() - Duration::from_secs(age))?
        .as_millis()
        .max(1);
    let headers = aterm_json::to_string(
        &json!({"x-pm-uid":"owned-uid","authorization":"Bearer owned-access"}),
    )
    .unwrap();
    let local = aterm_json::to_string("0").unwrap();
    let (url, script, body) = if kind == 0 {
        (
            API_URL,
            api_request_script(
                &aterm_json::to_string(API_URL).unwrap(),
                &aterm_json::to_string("GET").unwrap(),
                "undefined",
                &headers,
                wait,
            ),
            aterm_json::to_vec(&json!({"Code":1000,"Receipt":"owned-native-api"})).unwrap(),
        )
    } else {
        (
            KEY_URL,
            mailbox_key_script(&local, &headers, wait),
            aterm_json::to_vec(&json!({"Code":1000,"ClientKey":key.clone()})).unwrap(),
        )
    };
    let promise = page.start(&script);
    let request = page.paused(url);
    let actual = request["request"]["headers"].as_object().unwrap();
    for (name, wanted) in [
        ("x-pm-uid", "owned-uid"),
        ("authorization", "Bearer owned-access"),
    ] {
        assert!(
            actual.iter().any(
                |(key, value)| key.eq_ignore_ascii_case(name) && value.as_str() == Some(wanted)
            )
        );
    }
    page.fulfill(&request, "application/json", &body).unwrap();
    let outcome = page.outcome(&promise);
    assert_eq!(
        outcome["ok"].as_bool(),
        Some(true),
        "native page kind{kind}: {outcome:?}"
    );
    if kind == 0 {
        assert_eq!(outcome["value"]["status"].as_u64(), Some(200));
    } else {
        assert_eq!(outcome["value"].as_str(), Some("owned-mail-key"));
    }
    Ok(outcome)
}

#[test]
fn required_native_page_responses_keep_the_original_operation() {
    let mut page = Page::new();
    let key = install_session(&mut page);
    for kind in [0, 1] {
        for age in [26, 61, 0] {
            let got = response(&mut page, &key, kind, age, None);
            assert_eq!(
                got.is_ok(),
                age != 61,
                "native page kind{kind}, age{age}: {got:?}"
            );
            assert!(
                page.cdp
                    .take_events("Fetch.requestPaused", Some(&page.session))
                    .is_empty(),
                "an expired operation sends no browser request"
            );
            if age == 61 {
                response(&mut page, &key, kind, 0, None).unwrap();
            }
        }
        assert!(response(&mut page, &key, kind, 0, Some(Instant::now())).is_err());
        assert!(
            page.cdp
                .take_events("Fetch.requestPaused", Some(&page.session))
                .is_empty()
        );
        response(&mut page, &key, kind, 0, None).unwrap();
    }
}

#[test]
fn a_longer_native_page_allowance_keeps_origin_session_and_crypto_checks() {
    let mut page = Page::new();
    let key = install_session(&mut page);
    let wait = page_request_remaining(None, Instant::now() - Duration::from_secs(26))
        .unwrap()
        .as_millis();
    for url in [
        "https://other.invalid/api/mail/v4/messages",
        "https://mail.proton.me/outside",
    ] {
        let script = api_request_script(
            &aterm_json::to_string(url).unwrap(),
            "\"GET\"",
            "undefined",
            "{}",
            wait,
        );
        let promise = page.start(&script);
        let got = page.outcome(&promise);
        assert_eq!(got["ok"].as_bool(), Some(false));
        assert_eq!(got["message"].as_str(), Some("Invalid mail API URL"));
    }
    let script = mailbox_key_script("\"0\"", "{\"x-pm-uid\":\"other-uid\"}", wait);
    let promise = page.start(&script);
    let got = page.outcome(&promise);
    assert_eq!(got["ok"].as_bool(), Some(false));
    assert_eq!(got["message"].as_str(), Some("Different session"));
    assert!(
        page.cdp
            .take_events("Fetch.requestPaused", Some(&page.session))
            .is_empty()
    );
    let script = mailbox_key_script("\"0\"", "{\"x-pm-uid\":\"owned-uid\"}", wait);
    let promise = page.start(&script);
    let request = page.paused(KEY_URL);
    page.fulfill(
        &request,
        "application/json",
        &aterm_json::to_vec(
            &json!({"Code":1000,"ClientKey":aterm_codec::base64::encode(&[8;32]).unwrap()}),
        )
        .unwrap(),
    )
    .unwrap();
    let got = page.outcome(&promise);
    assert_eq!(
        got["ok"].as_bool(),
        Some(false),
        "native AES-GCM must reject a different key: {got:?}"
    );
    response(&mut page, &key, 1, 0, None).unwrap();
}
