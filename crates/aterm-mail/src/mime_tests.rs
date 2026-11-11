// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn html_numeric_entities_and_invisible_preview_padding_do_not_obscure_mail() {
    assert_eq!(
        html_text(
            "<p>Hello &#x1f44b; &#8212; &amp; welcome</p>&#8199;&#847;&shy;&#8199;<p>Readable &ldquo;mail&rdquo;</p>"
        ),
        "Hello 👋 — & welcome\nReadable “mail”\n"
    );
    assert_eq!(
        html_text("&amp;lt; &#x110000; &#0; &unknown;"),
        "&lt; &#x110000; &#0; &unknown;"
    );
}

#[test]
fn prefers_plain_part_and_decodes_headers_without_loading_html() {
    let raw = b"Subject: =?UTF-8?Q?Hello_=E2=9C=93?=\r\nContent-Type: multipart/alternative; boundary=x\r\n\r\n--x\r\nContent-Type: text/html\r\n\r\n<script>bad()</script><p>html</p>\r\n--x\r\nContent-Type: text/plain\r\nContent-Transfer-Encoding: base64\r\n\r\ncGxhaW4=\r\n--x--\r\n";
    let message = parse(raw).unwrap();
    assert_eq!(message.subject, "Hello ✓");
    assert_eq!(message.text, "plain");
    assert_eq!(
        html_text("<script>secret</script><p>hello &amp; goodbye</p>"),
        "hello & goodbye\n"
    );
}

#[test]
fn split_utf8_encoded_words_nested_head_style_and_boundary_crlf_decode_cleanly() {
    // Multi-byte UTF-8 character ('日' = E6 97 A5) split across adjacent RFC 2047 Q-words,
    // RFC 2231 language tag (`*en`), and malformed `=?` prefix before a valid encoded-word.
    assert_eq!(words("=?UTF-8?Q?=E6=97?= =?UTF-8?Q?=A5?="), "日");
    assert_eq!(words("=?iso-8859-1*fr?Q?caf=E9?="), "café");
    assert_eq!(
        words("=?broken word =?UTF-8?B?SGVsbG8=?="),
        "=?broken word Hello"
    );

    // Nested `<style>` inside `<head>`, self-closing `<style/>`, and Outlook MSO comments.
    assert_eq!(
        html_text(
            "<!--[if mso]> > <style> <![endif]--><head><style>body{}</style><title>Hidden</title></head><style type=\"text/css\"/><body><p>Code 654321</p></body>"
        ),
        "Code 654321\n"
    );

    // RFC 2046 §5.1.1: CRLF immediately preceding `--boundary` belongs to the boundary.
    let raw = b"Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nexact-line\r\n--b--\r\n";
    assert_eq!(parse(raw).unwrap().text, "exact-line");
}

/// NEGATIVE CONTROL: returning early on the first `text/plain` part in every `multipart/*`
/// drops the message body after a `text/plain` banner in `multipart/mixed`, and checking only
/// explicit `text/plain` in `multipart/alternative` concatenates an untyped default-plain part
/// with its `text/html` sibling.
#[test]
fn multipart_mixed_keeps_body_after_banner_while_alternative_prefers_untyped_plain() {
    let mixed = b"Content-Type: multipart/mixed; boundary=\"outer\" (gateway)\r\n\r\n\
--outer\r\n\
Content-Type: text/plain\r\n\r\n\
[External Email]\r\n\
--outer\r\n\
Content-Type: multipart/alternative; boundary=inner\r\n\r\n\
--inner\r\n\
\r\n\
Actual plain body\r\n\
--inner\r\n\
Content-Type: text/html\r\n\r\n\
<p>Duplicate HTML body</p>\r\n\
--inner--\r\n\
--outer\r\n\
Content-Type: text/plain\r\n\
Content-Transfer-Encoding: base64 (RFC 2045)\r\n\r\n\
Rm9vdGVy\r\n\
--outer--\r\n";
    assert_eq!(
        parse(mixed).unwrap().text,
        "[External Email]\nActual plain body\nFooter"
    );
}
