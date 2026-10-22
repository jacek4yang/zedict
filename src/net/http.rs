//! 共享 HTTP 客户端 (LLM / Edge 翻译 / 发音兜底 复用同一连接池)

use std::time::Duration;

/// Edge 翻译与 TTS 端点都会校验浏览器 UA，这里统一带一个稳定的桌面 UA。
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                              (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0";

static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();

/// 连接 10s / 读 30s / 总 180s，与原 LLM 客户端保持一致。
/// 单次请求可用 `RequestBuilder::timeout` 覆盖总时长。
pub fn client() -> reqwest::Client {
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .read_timeout(Duration::from_secs(30))
                .timeout(Duration::from_secs(180))
                .user_agent(USER_AGENT)
                .build()
                .unwrap_or_default()
        })
        .clone()
}

/// RFC 3986 百分号编码 (不引入新依赖；用于查询串与音频 URL)
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 语言代码只允许字母数字与连字符；配合 percent_encode 双重保险。
pub fn valid_lang(s: &str) -> bool {
    !s.is_empty() && s.len() <= 20 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

pub(crate) fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_everything_outside_the_unreserved_set() {
        assert_eq!(percent_encode("zh-Hans"), "zh-Hans");
        assert_eq!(percent_encode("a b"), "a%20b");
        assert_eq!(percent_encode("é"), "%C3%A9");
        assert_eq!(percent_encode("a&b+c"), "a%26b%2Bc");
        assert_eq!(percent_encode("😀"), "%F0%9F%98%80");
        assert_eq!(percent_encode("a/b?c=d#e"), "a%2Fb%3Fc%3Dd%23e");
    }

    #[test]
    fn rejects_loose_language_codes() {
        assert!(valid_lang("en"));
        assert!(valid_lang("zh-Hans"));
        assert!(!valid_lang(""));
        assert!(!valid_lang("en?to=zh"));
        assert!(!valid_lang(&"a".repeat(21)));
    }

    #[test]
    fn truncation_counts_chars_not_bytes() {
        assert_eq!(truncate("我有笔。", 2), "我有…");
        assert_eq!(truncate("abc", 9), "abc");
    }
}
