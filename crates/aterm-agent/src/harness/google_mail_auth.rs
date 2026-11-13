// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Google-only account selection and password entry around the bounded OAuth reader.

pub(super) fn step_js(
    fallback: &str,
    email: Option<&str>,
    password: Option<&str>,
    submitted: bool,
    code_is_authenticator: bool,
) -> String {
    let quoted = |text: &str| aterm_json::to_string(text).unwrap_or_else(|_| "\"\"".into());
    format!(
        r#"(() => {{
      const url = String(window.location.href);
      if (window.location.origin !== 'https://accounts.google.com') {{
        if (url.startsWith('http://127.0.0.1:') || url.startsWith('chrome-error://')) return {fallback};
        return {{state:'google_refused',url}};
      }}
      const visible = e => e && !e.disabled && e.getClientRects().length > 0;
      if (Array.from(document.querySelectorAll('iframe[src*="recaptcha"], input[name="ca"], #captchaimg')).some(visible)) return {{state:'google_refused',url}};
      const email = {email};
      const choices = Array.from(document.querySelectorAll('[data-identifier]'));
      const choice = choices.find(e => visible(e) && String(e.getAttribute('data-identifier')).toLowerCase() === email.toLowerCase());
      if (choice) {{ choice.click(); return {{state:'clicked_consent',url}}; }}
      const identities = Array.from(document.querySelectorAll('[data-email], [data-identifier], #profileIdentifier'));
      const matches = email.length > 0 && identities.some(e => visible(e) && String(e.getAttribute('data-email') || e.getAttribute('data-identifier') || e.textContent).trim().toLowerCase() === email.toLowerCase());
      const field = Array.from(document.querySelectorAll('input[type="password"]')).find(visible);
      if (field) {{
        if (!matches) return {{state:'google_refused',url}};
        if ({submitted}) return {{state:'loading',url}};
        const password = {password};
        if (!password) return {{state:'google_no_password',url}};
        const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set;
        setter.call(field,password);
        field.dispatchEvent(new Event('input',{{bubbles:true}}));
        field.dispatchEvent(new Event('change',{{bubbles:true}}));
        const next = document.querySelector('#passwordNext');
        if (!visible(next)) return {{state:'loading',url}};
        next.click(); return {{state:'submitted_password',url}};
      }}
      const authenticator = Array.from(document.querySelectorAll('input[name="totpPin"], #totpPin')).some(visible);
      const sms = Array.from(document.querySelectorAll('input[name="smsPin"], #smsPin')).some(visible) || /\/challenge\/(?:sms|ipp)(?:[/?#]|$)/.test(url);
      if (sms || (authenticator && !matches)) return {{state:'google_refused',url}};
      if (authenticator && !{code_is_authenticator}) return {{state:'google_needs_totp',url}};
      const emailCode = Array.from(document.querySelectorAll('input[autocomplete="one-time-code" i], input[name*="code" i], input[name*="otp" i], input[inputmode="numeric" i], input[data-testid*="otp" i], input[maxlength="1"], input[name="idvPin"], #idvPin')).some(visible);
      if (emailCode && !matches) return {{state:'google_refused',url}};
      if (!authenticator && emailCode && {code_is_authenticator}) return {{state:'needs_mailbox',url}};
      const result = {fallback};
      if (authenticator && result.state === 'needs_mailbox') return {{state:'google_needs_totp',url}};
      return result;
    }})()"#,
        email = quoted(email.unwrap_or("")),
        password = quoted(password.unwrap_or(""))
    )
}

#[cfg(test)]
#[path = "google_mail_auth_tests.rs"]
mod tests;
