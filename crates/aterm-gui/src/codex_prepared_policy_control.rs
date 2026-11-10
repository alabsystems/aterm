// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Private immutable policy observation. No new work or credential access.

use super::*;

fn operation(line: &str) -> Option<&str> {
    if line.len() > MAX_FRAME {
        return None;
    }
    let mut words = line.split_ascii_whitespace();
    if words.next()? != "assignment-target" {
        return None;
    }
    let mut fields = BTreeMap::new();
    for word in words {
        let (key, value) = word.split_once('=')?;
        if value.is_empty() || fields.insert(key, value).is_some() {
            return None;
        }
    }
    if fields.remove("schema")? != "1" {
        return None;
    }
    let operation = fields.remove("operation")?;
    (hex(operation, 32) && fields.is_empty()).then_some(operation)
}

fn error(why: registry::Error) -> String {
    match why {
        registry::Error::Busy => "ERR codex-accounts-change busy\n",
        registry::Error::Denied => "ERR codex-accounts-change denied\n",
        registry::Error::Stale => "ERR codex-accounts-change stale\n",
        _ => "ERR codex-accounts-change unavailable\n",
    }
    .into()
}

pub(in crate::native_act) fn serve(
    ctx: &Arc<crate::SessionCtx>,
    master: i32,
    line: &str,
    registry: &Arc<registry::Registry>,
    connection_id: Option<u64>,
) -> String {
    let Some(operation) = operation(line) else {
        return "ERR codex-accounts-change invalid request\n".into();
    };
    let Some(id) = connection_id.filter(|id| *id != 0) else {
        return error(registry::Error::Denied);
    };
    let (caller, peer) = match claim(ctx, master, registry) {
        Ok(value) => value,
        Err(why) => return error(why),
    };
    let Some(epoch) = host_epoch() else {
        return error(registry::Error::Unavailable);
    };
    let connection = match registry.connection(id) {
        Ok(value) if value.is_live() => value,
        Ok(_) => return error(registry::Error::Stale),
        Err(why) => return error(why),
    };
    let preview = match registry.prepared_policy(&peer, operation) {
        Ok(value) => value,
        Err(why) => return error(why),
    };
    let Ok(mut value) = aterm_json::to_value(&preview) else {
        return error(registry::Error::Unavailable);
    };
    let Some(object) = value.as_object_mut() else {
        return error(registry::Error::Unavailable);
    };
    object.insert("schema".into(), aterm_json::json!(1));
    object.insert("session".into(), session(&caller, epoch));
    if !connection.is_live()
        || !claim(ctx, master, registry).is_ok_and(|(fresh, _)| fresh == caller)
    {
        return error(registry::Error::Stale);
    }
    let Ok(body) = aterm_json::to_string(&value) else {
        return error(registry::Error::Unavailable);
    };
    let reply = format!("OK codex-accounts-change assignment-target {body}\n");
    if reply.len() > MAX_FRAME {
        return error(registry::Error::Unavailable);
    }
    reply
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_request_is_closed_and_has_no_caller_supplied_policy() {
        let operation_id = "a".repeat(32);
        let valid = format!("assignment-target schema=1 operation={operation_id}");
        assert_eq!(operation(&valid), Some(operation_id.as_str()));
        for extra in [
            "schema=1",
            "member=none",
            "target_provider=unknown",
            "capability=other",
            "source_fedramp=false",
        ] {
            assert!(operation(&format!("{valid} {extra}")).is_none());
        }
        assert!(operation(&valid.replace("schema=1", "schema=2")).is_none());
        assert!(operation(&valid.replace(&operation_id, &"A".repeat(32))).is_none());
        assert!(operation(&format!("{valid}{}", " ".repeat(MAX_FRAME))).is_none());
    }
}
