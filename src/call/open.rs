//! 新开一个 ACP 会话（设计 73 第四节第 2～5 条）：找到、装好 agent → 起进程 → `initialize` →（供应商填了 key 的）`authenticate`
//! → `session/new`（系统提示词照能力交、Miyu 的工具经 `mcpServers` 交、认得的 agent 关掉自带的工具）→ 选模型 → 权限模式切到
//! 「每次都问」（[`modes`]，施工 ACP-10）→ 整份历史转写成第一次提问。成功了、认得出对话的登记进表，agent 留着等下一轮。
//!
//! 要登录时（`session/new` 回 `-32000`，或者 `session/prompt` 回）这一次回 `auth`：说清这个 agent 要先登录、登录方式有哪些、
//! 去哪看；不替主人走 OAuth，不碰别家程序的凭据。

use std::fmt::Write;
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};

use super::talk::{Ended, talk};
use super::{Job, Signal, cancelled, keep, succeeded, with_sent};
use crate::acp::handshake::{self, Caps, SystemMode};
use crate::acp::modes;
use crate::acp::{Conn, Failure};
use crate::agents::{self, Agent};
use crate::failure::{self, Class};
use crate::live::{self, Live, Opened};
use crate::models::{self, Learned};
use crate::prompt::{self, Media};
use crate::sessions::Incoming;
use crate::turn::{Turn, failed};
use crate::{catalog, words, workspace};

/// 握手、开会话这几步各自至多等多久（npx 头一次要下载包，留足）。
pub const SETUP_WAIT: Duration = Duration::from_mins(3);

/// 改 [`SETUP_WAIT`] 的环境变量（毫秒）：只给测试用。
pub const SETUP_WAIT_ENV: &str = "MIYU_ACP_SETUP_WAIT_MS";

fn setup_wait() -> Duration {
    std::env::var(SETUP_WAIT_ENV)
        .ok()
        .and_then(|text| text.parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map_or(SETUP_WAIT, Duration::from_millis)
}

/// 新开一段：成功了、认得出对话（`incoming`）的登记进表。交回答。
pub(super) fn fresh(job: &Job<'_>, incoming: Option<Incoming>) -> Value {
    let agent = match find(job) {
        Ok(agent) => agent,
        Err(answer) => return answer,
    };
    let (mut live, sent_system) = match open(job, &agent) {
        Ok(opened) => opened,
        Err(answer) => return answer,
    };
    let media = Media::new(job.params, &live.caps);
    let prompt = match prompt::fresh(job.request, &media, sent_system.as_deref()) {
        Ok(prompt) => prompt,
        Err(error) => return failed(Class::Unclassified, &error.to_string()),
    };
    let params = json!({"sessionId": live.session, "prompt": prompt});
    let mut turn = Turn::new(job.names.clone(), &live.login).speaking(&job.config.language);
    let ended = talk(job, &mut live, &mut turn, |live| send_prompt(live, &params));
    learn_window(job, &turn);
    let (answer, pending) = match ended {
        Ended::Handed(answer, pending) => (answer, Some(pending)),
        other => (other.into_answer(), None),
    };
    if !succeeded(&answer) {
        return answer;
    }
    let handed = pending.is_some();
    if let Some(pending) = pending {
        live.set_pending(pending);
    }
    keep(job, incoming, live, handed);
    with_sent(answer, &params["prompt"])
}

/// 发这一轮的 `session/prompt`（回应送进信号），记下编号。
pub(super) fn send_prompt(live: &mut Live, params: &Value) -> Result<(), String> {
    let id = live
        .conn()
        .request_routed("session/prompt", params)
        .ok_or("the agent stopped taking input")?;
    live.prompt = Some(id);
    Ok(())
}

/// 记下这一轮学到的窗口。
pub(super) fn learn_window(job: &Job<'_>, turn: &Turn) {
    if let (Some(window), Ok(state)) = (turn.window(), workspace::state_dir()) {
        models::learn(
            &state,
            &job.agent,
            &Learned {
                window: Some((job.model.clone(), window)),
                resolved: turn.model().map(str::to_string),
                ..Learned::default()
            },
        );
    }
}

/// 找到、装好 agent。装的时候核心取消了的不等它装完（装的线程接着装，下一次用得上）。
fn find(job: &Job<'_>) -> Result<Agent, Value> {
    let (state, cache) = workspace::state_dir()
        .and_then(|state| workspace::cache_dir().map(|cache| (state, cache)))
        .map_err(|error| {
            failed(
                Class::Unclassified,
                &format!("no state folder for the ACP agents: {error}"),
            )
        })?;
    let (sender, receiver) = mpsc::channel();
    let (id, config) = (job.agent.clone(), job.config.clone());
    std::thread::spawn(move || {
        let found = agents::resolve(&id, &config, || catalog::load_now(&cache), &state, &cache);
        sender.send(found).ok();
    });
    loop {
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(found) => return found.map_err(|why| failed(Class::Unclassified, &why)),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if matches!(job.signals.try_recv(), Ok(Signal::Cancel)) {
                    return Err(cancelled());
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(failed(
                    Class::Unclassified,
                    "installing the agent failed unexpectedly",
                ));
            }
        }
    }
}

/// 起进程、握手、开会话、选模型。交活会话和要放进第一次提问的系统提示词（agent 不收系统提示词时）。
fn open(job: &Job<'_>, agent: &Agent) -> Result<(Live, Option<String>), Value> {
    let work = workspace::work_dir().map_err(|error| {
        failed(
            Class::Unclassified,
            &format!("could not make a working folder for the agent: {error}"),
        )
    })?;
    let wiring = if job.declared.is_empty() {
        None
    } else {
        Some(
            job.tools
                .wire(
                    job.declared.clone(),
                    job.family == agents::Family::Antigravity,
                )
                .map_err(|why| failed(Class::Unclassified, &why))?,
        )
    };
    let (route, stray) = live::plumbing(wiring.as_ref());
    let conn = Conn::start(&agent.launch, &work, route, stray).map_err(|error| {
        failed(
            Class::Unclassified,
            &format!(
                "could not start {} ({}): {error}",
                agent.name,
                agent.launch.program.display()
            ),
        )
    })?;
    let wait = setup_wait();
    let step = |method: &str, params: &Value| conn.request(method, params, wait, job.signals);
    let caps = shake_hands(job, agent, &step)?;
    let title = caps.title.clone().unwrap_or_else(|| agent.name.clone());
    let login = login_help(agent, &title, &caps);
    let system = format!(
        "{}\n\n{}",
        job.request
            .get("system")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        words::WORKSPACE_NOTE
    );
    let mcp = wiring.as_ref().map(|wiring| {
        let program = std::env::current_exe().unwrap_or_else(|_| "miyu-acp".into());
        handshake::mcp_server(&program, wiring.socket(), wiring.token())
    });
    let params = handshake::session_params(
        &work,
        mcp,
        &caps,
        &system,
        agent.family,
        job.config.own_tools,
    );
    let session = match step("session/new", &params) {
        Ok(result) => result,
        Err(Failure::Rpc(error)) if error.needs_login() => return Err(failed(Class::Auth, &login)),
        Err(failure) => return Err(setup_failed(agent, "session/new", failure)),
    };
    let found = handshake::read_session(&session).ok_or_else(|| {
        failed(
            Class::Unclassified,
            &format!("{} opened a session without a sessionId", agent.name),
        )
    })?;
    learn(job, &caps, &found.models);
    if let Some(model) = &job.model {
        pick_model(agent, &found, model, &step)?;
    }
    ask_every_time(agent, &found.session, &session, &step)?;
    let in_prompt = (caps.system == SystemMode::InPrompt).then_some(system);
    let opened = Opened {
        session: found.session,
        agent: agent.id.clone(),
        title,
        caps,
        login,
    };
    Ok((Live::new(conn, opened, wiring), in_prompt))
}

/// 发一个请求、等回应（握手、开会话这几步用）。
type Step<'a> = dyn Fn(&str, &Value) -> Result<Value, Failure> + 'a;

/// `initialize`，核对协议版本；供应商填了 key、agent 有用密钥登录的方式的，`authenticate`。
fn shake_hands(job: &Job<'_>, agent: &Agent, step: &Step<'_>) -> Result<Caps, Value> {
    let caps = handshake::read_caps(
        &step("initialize", &handshake::initialize_params())
            .map_err(|f| setup_failed(agent, "initialize", f))?,
    );
    if caps.protocol != handshake::PROTOCOL_VERSION {
        return Err(failed(
            Class::Unclassified,
            &format!(
                "{} speaks ACP version {}, but Miyu speaks version {}",
                agent.name,
                caps.protocol,
                handshake::PROTOCOL_VERSION
            ),
        ));
    }
    if let (Some(key), Some(method)) = (
        job.params.get("key").and_then(Value::as_str),
        handshake::api_key_method(&caps),
    ) {
        step("authenticate", &handshake::api_key_params(method, key))
            .map_err(|f| setup_failed(agent, "authenticate", f))?;
    }
    Ok(caps)
}

/// 选模型 `model`：已经是它的不用选；agent 列了模型、里面没有它的报错。
fn pick_model(
    agent: &Agent,
    found: &handshake::NewSession,
    model: &str,
    step: &Step<'_>,
) -> Result<(), Value> {
    if found.current.as_deref() == Some(model) {
        return Ok(());
    }
    if !found.models.is_empty() && !found.models.iter().any(|offered| offered == model) {
        return Err(failed(
            Class::Unclassified,
            &format!(
                "{} does not offer the model {model:?}; it offers {}",
                agent.name,
                found.models.join(", ")
            ),
        ));
    }
    let (method, params) = handshake::select_model(found, model);
    step(method, &params).map_err(|f| setup_failed(agent, method, f))?;
    Ok(())
}

/// 对方报了权限模式的，切到「每次都问」的那一档（[`modes`]）。切不了、认不出哪一档的照原样用、日志里记一句：对方之后不要权限
/// 就跑了的，供应商侧工具块上写明「未经确认已执行」（`turn/hosted.rs`）。核心取消了、agent 退了的照开会话没成。
fn ask_every_time(
    agent: &Agent,
    session: &str,
    result: &Value,
    step: &Step<'_>,
) -> Result<(), Value> {
    let Some(found) = modes::read(result) else {
        return Ok(());
    };
    let Some(target) = modes::asking(&found) else {
        eprintln!(
            "{} offers no mode that always asks ({}); keeping {}",
            agent.name,
            found.available.join(", "),
            found.current.as_deref().unwrap_or("its default")
        );
        return Ok(());
    };
    let Some((method, params)) = modes::switch(session, &found, target) else {
        return Ok(());
    };
    match step(method, &params) {
        Ok(_) => Ok(()),
        Err(failure @ (Failure::Cancelled | Failure::Closed)) => {
            Err(setup_failed(agent, method, failure))
        }
        Err(failure) => {
            eprintln!(
                "{} did not switch to the mode {target:?} ({failure:?}); keeping {}",
                agent.name,
                found.current.as_deref().unwrap_or("its default")
            );
            Ok(())
        }
    }
}

/// 记下 agent 报的模型、收不收图。
fn learn(job: &Job<'_>, caps: &Caps, models: &[String]) {
    if let Ok(state) = workspace::state_dir() {
        models::learn(
            &state,
            &job.agent,
            &Learned {
                models: (!models.is_empty()).then(|| models.to_vec()),
                vision: Some(caps.prompt.image),
                ..Learned::default()
            },
        );
    }
}

/// 握手、开会话的一步没成：照原因归类。
fn setup_failed(agent: &Agent, step: &str, failure: Failure) -> Value {
    match failure {
        Failure::Cancelled => cancelled(),
        Failure::Closed => failed(
            Class::Retryable,
            &format!("{} exited during {step}", agent.name),
        ),
        Failure::Timeout => failed(
            Class::Retryable,
            &format!("{} did not answer {step} in time", agent.name),
        ),
        Failure::Rpc(error) => {
            let said = error.said();
            failed(
                failure::classify(&said, false),
                &format!("{} refused {step}: {said}", agent.name),
            )
        }
    }
}

/// 要登录时给人看的一句（英文；核心截到 500 个字给人看）。
fn login_help(agent: &Agent, title: &str, caps: &Caps) -> String {
    let mut text =
        format!("{title} needs you to log in first. Log in with the agent's own program");
    if !caps.auth_methods.is_empty() {
        let names: Vec<&str> = caps
            .auth_methods
            .iter()
            .map(|method| method.name.as_str())
            .collect();
        let _ = write!(text, " (it offers: {})", names.join(", "));
    }
    if let Some(site) = &agent.website {
        let _ = write!(text, "; see {site}");
    }
    if handshake::api_key_method(caps).is_some() {
        text.push_str("; or put an API key in this provider's key setting");
    }
    text.push_str(". Miyu does not log in for you.");
    text
}
