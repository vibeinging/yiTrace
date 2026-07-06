fn json_field_alias<'a>(
    obj: &'a crate::wire::Json,
    names: &[&str],
) -> Option<&'a crate::wire::Json> {
    names.iter().find_map(|name| crate::wire::field(obj, name))
}

fn json_raw_field_alias<'a>(
    obj: &'a crate::wire::Json,
    names: &[&str],
) -> Option<&'a crate::wire::Json> {
    names.iter().find_map(|name| obj.get(name))
}

fn optional_string_patch(obj: &crate::wire::Json, names: &[&str]) -> Option<Option<String>> {
    json_raw_field_alias(obj, names).and_then(|value| match value {
        crate::wire::Json::Null => Some(None),
        _ => value.as_str().map(|s| Some(s.to_string())),
    })
}

fn optional_score_patch(obj: &crate::wire::Json, names: &[&str]) -> Option<Option<u32>> {
    json_raw_field_alias(obj, names).and_then(|value| match value {
        crate::wire::Json::Null => Some(None),
        _ => value.as_u64().map(|n| Some(n.min(u32::MAX as u64) as u32)),
    })
}

fn json_bool_alias(obj: &crate::wire::Json, names: &[&str]) -> Option<bool> {
    json_field_alias(obj, names).and_then(|value| match value {
        crate::wire::Json::Bool(v) => Some(*v),
        crate::wire::Json::Num(s) | crate::wire::Json::Str(s) => {
            if s.eq_ignore_ascii_case("true") || s == "1" {
                Some(true)
            } else if s.eq_ignore_ascii_case("false") || s == "0" {
                Some(false)
            } else {
                None
            }
        }
        _ => None,
    })
}

fn json_cost_nanos_alias(
    obj: &crate::wire::Json,
    nanos_names: &[&str],
    usd_names: &[&str],
) -> Option<u64> {
    json_field_alias(obj, nanos_names)
        .and_then(crate::wire::Json::as_u64)
        .or_else(|| {
            json_field_alias(obj, usd_names)
                .and_then(crate::wire::Json::as_f64)
                .and_then(|value| {
                    if value.is_finite() && value >= 0.0 {
                        Some((value * 1_000_000_000.0).round().min(u64::MAX as f64) as u64)
                    } else {
                        None
                    }
                })
        })
}

fn json_string_list_alias(obj: &crate::wire::Json, names: &[&str]) -> Vec<String> {
    let Some(value) = json_field_alias(obj, names) else {
        return Vec::new();
    };
    match value {
        crate::wire::Json::Arr(items) => items
            .iter()
            .filter_map(crate::wire::Json::as_str)
            .map(ToString::to_string)
            .collect(),
        crate::wire::Json::Str(s) if !s.trim().is_empty() => s
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(ToString::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn query_bool(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "y" | "on"
    )
}
