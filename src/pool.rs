//! 扩展进程里那一张活着的 ACP 会话的登记表（[`sessions::Table`] 加锁）和闲置回收的巡检线程。照 `miyu-antigravity` 的
//! `registry` 搬（改名是免得和 ACP 的「登记表」[`crate::catalog`] 撞名）。
//!
//! 挂着的项（一轮以交回结束、agent 还等着结果）和闲着的一样留着活进程，不同的是闲置的上限用 `acp.tool_wait_seconds`。
//!
//! 收进程慢（[`crate::process::terminate`] 至多等几秒）：从表里拿出来的活进程一律在放开锁以后丢掉（丢掉就是收掉，见
//! [`Live`]）。

use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::live::Live;
use crate::sessions::{self, Claimed, Incoming, Meta, Table};

/// 巡检多久一次。
pub const SWEEP_EVERY: Duration = Duration::from_secs(15);

/// 改巡检间隔的环境变量（毫秒）：只给测试用，不是配置项。
pub const SWEEP_ENV: &str = "MIYU_ACP_SWEEP_MS";

/// 巡检间隔：环境变量 [`SWEEP_ENV`] 给了正整数的照它，别的照 [`SWEEP_EVERY`]。
#[must_use]
pub fn sweep_every() -> Duration {
    std::env::var(SWEEP_ENV)
        .ok()
        .and_then(|text| text.parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map_or(SWEEP_EVERY, Duration::from_millis)
}

/// 改挂着的工具调用等多久的环境变量（毫秒）：只给测试用，不是配置项。
pub const TOOL_WAIT_ENV: &str = "MIYU_ACP_TOOL_WAIT_MS";

/// 挂着的工具调用等多久：环境变量 [`TOOL_WAIT_ENV`] 给了正整数的照它，别的照配置的 `configured`。
#[must_use]
pub fn tool_wait(configured: Duration) -> Duration {
    std::env::var(TOOL_WAIT_ENV)
        .ok()
        .and_then(|text| text.parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map_or(configured, Duration::from_millis)
}

/// 活着的会话的表。
#[derive(Debug)]
pub struct Registry {
    table: Mutex<Table<Live>>,
}

impl Default for Registry {
    fn default() -> Registry {
        Registry {
            table: Mutex::new(Table::new(sessions::LIMIT)),
        }
    }
}

impl Registry {
    fn lock(&self) -> MutexGuard<'_, Table<Live>> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 找 `incoming` 接得上的闲着的（或者挂着的）一段，改成正忙、交出来（[`Table::claim`]）。先把进程已经退了的拿掉；
    /// 认完以后，被这一次请求顶掉的挂着的项也拿掉（[`Table::displace`]）。拿掉的都在放开锁以后收。
    #[must_use]
    pub fn claim(&self, incoming: &Incoming) -> Option<Claimed<Live>> {
        let (removed, claimed) = {
            let mut table = self.lock();
            let mut removed = table.sweep(Instant::now(), Duration::MAX, Live::closed);
            let claimed = table.claim(incoming);
            removed.extend(table.displace(incoming));
            (removed, claimed)
        };
        drop(removed);
        claimed
    }

    /// 正忙的 `id` 这一轮成功了：记下 `messages`，改回闲着（[`Table::release`]）。
    pub fn release(&self, id: u64, live: Live, messages: Vec<String>) {
        let removed = self.lock().release(id, live, messages, Instant::now());
        drop(removed);
    }

    /// 正忙的 `id` 这一轮以交回一件工具调用结束：记下 `messages`（交回它的这一次请求的），改成挂着的。
    pub fn release_pending(&self, id: u64, live: Live, messages: Vec<String>) {
        let removed = self
            .lock()
            .release_pending(id, live, messages, Instant::now());
        drop(removed);
    }

    /// 正忙的 `id` 出了错：从表里拿掉（进程由调用的人收）。
    pub fn forget(&self, id: u64) {
        self.lock().forget(id);
    }

    /// 登记一段新开的、说完了的对话（[`Table::insert`]）。
    pub fn insert(&self, meta: Meta, live: Live) {
        let removed = self.lock().insert(meta, live);
        drop(removed);
    }

    /// 巡检：收掉闲置超过 `keep` 的、挂着超过 `tool_wait` 的和进程自己退了的。
    pub fn sweep(&self, keep: Duration, tool_wait: Duration) {
        let removed = self
            .lock()
            .sweep_with(Instant::now(), keep, tool_wait, Live::closed);
        drop(removed);
    }

    /// 扩展要退出：收掉所有闲着的和挂着的。
    pub fn drain(&self) {
        let removed = self.lock().drain();
        drop(removed);
    }
}

/// 巡检线程：每 `every` 照现在的配置收一次（[`Registry::sweep`]）。
pub struct Sweeper {
    stop: Sender<()>,
    thread: JoinHandle<()>,
}

impl Sweeper {
    /// 起巡检线程。
    #[must_use]
    pub fn start(registry: Arc<Registry>, config: Arc<RwLock<Config>>, every: Duration) -> Sweeper {
        let (stop, stopped) = mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(every) {
                let (keep, wait) = {
                    let config = config.read().unwrap_or_else(PoisonError::into_inner);
                    (config.keep, tool_wait(config.tool_wait))
                };
                registry.sweep(keep, wait);
            }
        });
        Sweeper { stop, thread }
    }

    /// 停下巡检线程，等它结束。
    pub fn stop(self) {
        let Sweeper { stop, thread } = self;
        drop(stop);
        if thread.join().is_err() {
            eprintln!("the thread stopping idle agent processes panicked");
        }
    }
}
