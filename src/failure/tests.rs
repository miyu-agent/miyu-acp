use super::*;

/// 账号被停用时 agy 报的原话（2026-11-20 主人本机）。
const DISABLED: &str = "This service has been disabled in this account for violation of Terms of Service. Please submit an appeal to continue.";

/// Gemini 拒收整数 `enum` 时 agy 报的原话（2026-11-20 主人本机，im 包的 `qq_activity_ranking`）。
const INVALID_ENUM: &str = "INVALID_ARGUMENT (code 400): * GenerateContentRequest.tools[28].function_declarations[0].parameters.properties[days].enum: only allowed for STRING type";

#[test]
fn table() {
    let cases = [
        ("You are not logged into Antigravity", false, Class::Auth),
        ("UNAUTHENTICATED: token expired", true, Class::Auth),
        ("Please sign in again", false, Class::Auth),
        ("Quota exceeded for model gemini", false, Class::Quota),
        ("RESOURCE_EXHAUSTED", true, Class::Quota),
        ("You have reached your usage limit", false, Class::Quota),
        ("Out of credits", false, Class::Quota),
        ("Rate limit exceeded, slow down", false, Class::RateLimited),
        ("429 Too Many Requests", true, Class::RateLimited),
        ("model output error", false, Class::Retryable),
        ("model output error", true, Class::Unclassified),
        ("", false, Class::Retryable),
        // 几种都有的取靠前的：额度用完也常带「rate limit」。
        ("quota exhausted (rate limit)", false, Class::Quota),
        ("not logged in; quota unknown", false, Class::Auth),
        // 2026-11-20 主人本机撞上的两句原话（AG-7）：账号被停用、请求本身有错，再来一次也一样。
        (DISABLED, false, Class::Auth),
        (DISABLED, true, Class::Auth),
        (
            "PERMISSION_DENIED (code 403): caller lacks permission",
            false,
            Class::Auth,
        ),
        (INVALID_ENUM, false, Class::Unclassified),
        (INVALID_ENUM, true, Class::Unclassified),
        (
            "FAILED_PRECONDITION (code 400): User location is not supported",
            false,
            Class::Unclassified,
        ),
        (
            "NOT_FOUND (code 404): model gemini-x is not found",
            false,
            Class::Unclassified,
        ),
        // 请求有错的原话里点到的属性名不拿来归类：名字里带 quota 的工具写坏了，也不是额度用完。
        (
            "INVALID_ARGUMENT (code 400): * GenerateContentRequest.tools[3].function_declarations[0].parameters.properties[quota].enum: only allowed for STRING type",
            false,
            Class::Unclassified,
        ),
        // Anthropic、OpenAI 的几句（照它们的出错说明）。
        (
            "prompt is too long: 210000 tokens > 200000 maximum",
            true,
            Class::ContextTooLong,
        ),
        (
            "This model's maximum context length is 128000 tokens (context_length_exceeded)",
            false,
            Class::ContextTooLong,
        ),
        (
            "authentication_error: invalid x-api-key",
            false,
            Class::Auth,
        ),
        ("rate_limit_error: slow down", false, Class::RateLimited),
        // 5xx 照旧可以再来。
        (
            "UNAVAILABLE (code 503): the model is overloaded",
            false,
            Class::Retryable,
        ),
    ];
    // 一次列出全部对不上的，不停在头一条。
    let wrong: Vec<String> = cases
        .into_iter()
        .filter(|(said, delivered, class)| classify(said, *delivered) != *class)
        .map(|(said, delivered, class)| {
            format!(
                "{said:?} delivered={delivered}: got {:?}, want {class:?}",
                classify(said, delivered)
            )
        })
        .collect();
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// 上下文超长的 400（AG-7 续）：报成核心的 `context_too_long`，核心先压缩再来；别的 400 照旧不算进来。按协议里的写法比，
/// 不靠 [`Class`] 的变体名。
#[test]
fn context_too_long_is_told_apart_from_other_400s() {
    let long = [
        "INVALID_ARGUMENT (code 400): The input token count (1337419) exceeds the maximum number of tokens allowed (1048576).",
        "The input token count (35868) exceeds the maximum number of tokens allowed (32768).",
        "INVALID_ARGUMENT (code 400): Request exceeds the model's input token limit.",
        "400 Input request contains (32981) tokens, which is more than the max tokens limit allowed: 32768",
    ];
    let other = [
        INVALID_ENUM,
        "INVALID_ARGUMENT (code 400): * GenerateContentRequest.generation_config.max_output_tokens: max_output_tokens must be positive",
        "INVALID_ARGUMENT (code 400): Request contains an invalid argument.",
        "FAILED_PRECONDITION (code 400): User location is not supported",
    ];
    let mut wrong = Vec::new();
    for delivered in [false, true] {
        for said in long {
            let got = classify(said, delivered).as_str();
            if got != "context_too_long" {
                wrong.push(format!("{said:?} delivered={delivered}: got {got}"));
            }
        }
        for said in other {
            let got = classify(said, delivered).as_str();
            if got != "unclassified" {
                wrong.push(format!("{said:?} delivered={delivered}: got {got}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn otherwise_depends_on_whether_deltas_went_out() {
    assert_eq!(otherwise(false), Class::Retryable);
    assert_eq!(otherwise(true), Class::Unclassified);
}

#[test]
fn error_is_cut_to_500_chars() {
    let long = "错".repeat(600);
    let got = error(Class::Quota, &long);
    assert_eq!(got["class"], "quota");
    assert_eq!(got["message"].as_str().unwrap().chars().count(), 500);
    assert_eq!(error(Class::RateLimited, "x")["class"], "rate_limited");
    assert_eq!(error(Class::Retryable, "x")["class"], "retryable");
    assert_eq!(error(Class::Auth, "x")["class"], "auth");
    assert_eq!(error(Class::Unclassified, "x")["message"], "x");
}
