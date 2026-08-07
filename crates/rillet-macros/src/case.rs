//! Case conversions for generated identifier names.

/// Converts a CamelCase type name to snake_case, keeping acronym runs
/// together: `MessageSent` becomes `message_sent`, `HTTPServer` becomes
/// `http_server`.
pub fn to_snake_case(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut result = String::new();

    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() {
            let prev = i.checked_sub(1).map(|j| chars[j]);
            let next = chars.get(i + 1);
            let after_lower_or_digit = prev.is_some_and(|p| p.is_lowercase() || p.is_ascii_digit());
            let ends_acronym_run =
                prev.is_some_and(|p| p.is_uppercase()) && next.is_some_and(|n| n.is_lowercase());
            if after_lower_or_digit || ends_acronym_run {
                result.push('_');
            }
            result.extend(c.to_lowercase());
        } else {
            result.push(c);
        }
    }

    result
}

/// Converts a snake_case method name to PascalCase: `send_message`
/// becomes `SendMessage`.
pub fn to_pascal_case(s: &str) -> String {
    s.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_case_splits_camel_words() {
        assert_eq!(to_snake_case("MessageSent"), "message_sent");
        assert_eq!(to_snake_case("Chirp"), "chirp");
        assert_eq!(to_snake_case("already_snake"), "already_snake");
    }

    #[test]
    fn snake_case_keeps_acronym_runs_together() {
        assert_eq!(to_snake_case("HTTPServer"), "http_server");
        assert_eq!(to_snake_case("PeerID"), "peer_id");
        assert_eq!(to_snake_case("IOError"), "io_error");
        assert_eq!(to_snake_case("HTTP"), "http");
    }

    #[test]
    fn snake_case_splits_after_digits() {
        assert_eq!(to_snake_case("Layer2Started"), "layer2_started");
    }

    #[test]
    fn pascal_case_joins_snake_words() {
        assert_eq!(to_pascal_case("send_message"), "SendMessage");
        assert_eq!(to_pascal_case("tick"), "Tick");
    }
}
