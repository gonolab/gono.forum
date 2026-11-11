use std::{future::Future, time::Duration};

use anyhow::{Context, Result};
use gono_forum_web::{
    activities,
    cache::{Cache, GlobalCache},
    config::Config,
    db::Database,
    debug::driver::{DriverFilter, DriverQueryLayer},
    identity::{self, Identity},
    instance_root,
    mail::Mailer,
    media::Media,
    moderation, notifications, privacy, router,
};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::{EnvFilter, prelude::*};

// musl's allocator is slow under multi-threaded load; release packages are static musl builds. Source builds on
// macOS and glibc keep the system allocator.
#[cfg(target_env = "musl")]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Tasks whose result the user waits for on the page: upload processing, account e-mail, in-site notifications.
const INTERACTIVE_PERIOD: Duration = Duration::from_secs(1);
/// Tasks that only follow the wall clock: expiring sanctions, due activities. A few seconds of delay does not
/// affect the experience.
const DEADLINE_PERIOD: Duration = Duration::from_secs(5);
/// Long multi-stage tasks: data export and erasure, which the page already shows as "Processing".
const STAGED_PERIOD: Duration = Duration::from_secs(30);
/// Sweeping tasks: online records, attachment cleanup, whose deadlines are counted in days.
const SWEEP_PERIOD: Duration = Duration::from_secs(60);
/// Time left for background tasks to wrap up after shutdown; only on timeout do we stop waiting and abort.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);
/// Wake-up interval of the rank task: equal to the lower bound of `settings.ranks.refresh_seconds`; the actual
/// refresh interval is computed from the setting each round.
const RANKS_TICK: Duration = Duration::from_secs(10);

/// Background tasks read site settings from the in-process global cache shared with the router; a missing row or
/// bad value is reported as usual (this round fails, the next round retries).
async fn settings(
    global: &GlobalCache,
    db: &Database,
) -> Result<std::sync::Arc<gono_forum_web::runtime::SiteSettings>> {
    Ok(global
        .settings(db)
        .await
        .map_err(|error| match error {
            identity::IdentityError::Internal(inner) => inner,
            other => anyhow::anyhow!(other.to_string()),
        })?
        .settings
        .clone())
}

/// Starts a background polling task.
///
/// Each round first checks that Redis is available; if not, the round is skipped instead of falling back to
/// carrying on with MySQL alone. The request path does no such upfront probe; only a failed cache read turns into
/// a 503, see `cache::protect`. On the shutdown signal it waits for the current round to finish on its own before
/// exiting, never aborting midway through a transaction or lease. Failures are only logged and the next round
/// retries.
fn spawn_worker<F, Fut>(
    workers: &mut JoinSet<()>,
    shutdown: &CancellationToken,
    cache: Cache,
    name: &'static str,
    period: Duration,
    mut work: F,
) where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    let shutdown = shutdown.clone();
    workers.spawn(async move {
        let mut interval = tokio::time::interval(period);
        // When a round runs longer than the period, firing the missed ticks is pointless; just move on to the next
        // period.
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Log only once on pause and once on resume: at most one round per second, so logging every round would
        // flood the log with the same line.
        let mut paused = false;
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                _ = interval.tick() => {}
            }
            if let Err(error) = cache.ensure_available().await {
                if !paused {
                    tracing::warn!(error = %error.root_cause(), worker = name, "Redis unavailable; background task paused until Redis recovers");
                    paused = true;
                }
                continue;
            }
            if paused {
                tracing::info!(worker = name, "Redis recovered; background task resumed");
                paused = false;
            }
            if let Err(error) = work().await {
                tracing::warn!(%error, worker = name, "background task did not finish; will retry");
            }
        }
    });
}

#[tokio::main]
async fn main() -> Result<()> {
    if matches!(std::env::args().nth(1).as_deref(), Some("--version" | "-V")) {
        println!("gono-forum-web {}", gono_forum_web::VERSION);
        return Ok(());
    }
    if gono_forum_web::media::worker_entry()? {
        return Ok(());
    }
    gono_forum_web::install_crypto_provider();
    let root = instance_root()?;
    let override_path = std::env::var_os("GONOF_CONFIG").map(std::path::PathBuf::from);
    let logs = tracing_subscriber::fmt::layer().with_filter(
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "gono_forum_web=info,tower_http=info".into()),
    );
    // Page capture has its own filter (it only takes SQLx events while the debug panel is on and this request is
    // being captured), so users need not enable SQL console logging. The panel switch is an admin setting that can
    // be turned on at any time while running, so this layer is always installed.
    let driver = DriverQueryLayer.with_filter(DriverFilter);
    tracing_subscriber::registry()
        .with(logs)
        .with(driver)
        .init();
    let mut config = Config::read(&root, override_path.as_deref())?;
    let config_path = override_path
        .clone()
        .unwrap_or_else(|| Config::default_path(&root));
    // The socket is bound once: the setup wizard and then the forum serve on clones of it, so the browser that just
    // finished the wizard lands on the forum without a restart.
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .context("cannot listen on the local address")?
        .into_std()?;
    if gono_forum_web::setup::unconfigured(&config) {
        let wizard =
            gono_forum_web::setup::Wizard::configure(root.clone(), config_path.clone(), config);
        if let gono_forum_web::setup::Finished::Interrupted =
            wizard.run(listener.try_clone()?, signal()).await?
        {
            return Ok(());
        }
        config = Config::read(&root, override_path.as_deref())?;
    }
    config.validate()?;
    // Required services are verified first; if Redis is unavailable, neither the database nor the service state is
    // initialized. The exception is redis.enabled = false: single-machine single-process deployments, or local
    // profiling of SQL and timing.
    let cache = Cache::connect(&config.redis).await?;
    // The invalidation subscription of the in-process global cache follows the same rule as PING: with Redis enabled,
    // failing to subscribe refuses startup (docs/settings-and-caching.md, "Invalidation").
    let global = GlobalCache::start(cache.clone()).await?;
    if !cache.enabled() {
        tracing::warn!(
            "redis.enabled = false: not connecting to Redis; every cache read goes through to the database and background tasks no longer wait for Redis. Suitable for single-machine single-process deployments and local profiling; multi-process deployments must enable it."
        );
    }
    let db = Database::connect(&config.database).await?;
    {
        // A long migration must not hit systemd's start timeout before READY=1.
        let extend = tokio::spawn(async {
            loop {
                gono_forum_web::systemd::extend_start(120);
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
        let migrated = db.migrate().await;
        extend.abort();
        migrated?;
    }
    // Forums are not created from config: a fresh install starts from an empty database and the administrator
    // creates them at /manage/forums.
    let identity = Identity::new(
        db.clone(),
        config.identity.clone(),
        root.clone(),
        global.clone(),
    );
    // Configured by hand (or the wizard was interrupted after writing the file) but no administrator yet: the wizard's
    // administrator stage takes the place of the forum until one is created.
    if identity.needs_admin().await? {
        let wizard = gono_forum_web::setup::Wizard::administrator(
            root.clone(),
            config_path.clone(),
            config.clone(),
            identity.clone(),
        );
        if let gono_forum_web::setup::Finished::Interrupted =
            wizard.run(listener.try_clone()?, signal()).await?
        {
            return Ok(());
        }
    }
    // Mail delivery, background workers and the router share one in-process global cache handle
    // (docs/settings-and-caching.md, "The process cache"): after a save in the admin area this process clears it
    // immediately, and workers read the new value in their next round.
    let mail = Mailer::new(
        db.clone(),
        config.common.clone(),
        root.join(&config.identity.local_directory).join("mail"),
        global.clone(),
    );
    let media =
        Media::new(db.clone(), config.media.clone(), root.clone()).with_global(global.clone());
    // config is then handed over to the router; the local directory is taken out first for the data request task.
    let privacy_directory = std::path::PathBuf::from(&config.identity.local_directory);
    // The cleanup keeps its directory walks from round to round, so one instance serves every round.
    let maintenance = std::sync::Arc::new(tokio::sync::Mutex::new(
        gono_forum_web::maintenance::Maintenance::new(root.join(&config.identity.local_directory)),
    ));

    let shutdown = CancellationToken::new();
    let mut workers = JoinSet::new();
    spawn_worker(
        &mut workers,
        &shutdown,
        cache.clone(),
        "account mail delivery",
        INTERACTIVE_PERIOD,
        move || {
            let mail = mail.clone();
            async move { mail.deliver_pending(identity::now()).await.map(drop) }
        },
    );
    spawn_worker(
        &mut workers,
        &shutdown,
        cache.clone(),
        "media processing",
        INTERACTIVE_PERIOD,
        {
            let media = media.clone();
            move || {
                let media = media.clone();
                async move {
                    // When idle, slow down gradually to at most one check every 5 seconds; new uploads in this
                    // process are processed immediately.
                    media
                        .process_when_due(identity::now())
                        .await
                        .map_err(anyhow::Error::new)
                }
            }
        },
    );
    // Attachment cleanup scans for expired purge_after; deadlines are counted in days, so one round per minute is
    // enough (the candidate query uses the `(lifecycle, purge_after)` index `gonof_attachment_purge_due`).
    spawn_worker(
        &mut workers,
        &shutdown,
        cache.clone(),
        "media cleanup",
        SWEEP_PERIOD,
        {
            let media = media.clone();
            move || {
                let media = media.clone();
                async move {
                    media
                        .cleanup(identity::now())
                        .await
                        .map(drop)
                        .map_err(anyhow::Error::new)
                }
            }
        },
    );
    spawn_worker(
        &mut workers,
        &shutdown,
        cache.clone(),
        "reply notification delivery",
        INTERACTIVE_PERIOD,
        {
            let db = db.clone();
            move || {
                let db = db.clone();
                async move {
                    notifications::deliver_pending(&db, identity::now())
                        .await
                        .map(drop)
                }
            }
        },
    );
    // Forum merge: each round claims one job and moves at most 5 batches of threads; when idle it
    // is just one bounded Outbox query.
    spawn_worker(
        &mut workers,
        &shutdown,
        cache.clone(),
        "forum merge",
        INTERACTIVE_PERIOD,
        {
            let db = db.clone();
            let global = global.clone();
            move || {
                let db = db.clone();
                let global = global.clone();
                async move {
                    gono_forum_web::organization::process_merges(&db, &global, identity::now())
                        .await
                        .map(drop)
                }
            }
        },
    );
    spawn_worker(
        &mut workers,
        &shutdown,
        cache.clone(),
        "data request processing",
        STAGED_PERIOD,
        {
            let db = db.clone();
            let root = root.clone();
            let global = global.clone();
            move || {
                let db = db.clone();
                let media = media.clone();
                let root = root.clone();
                let directory = privacy_directory.clone();
                let global = global.clone();
                async move {
                    let retention = settings(&global, &db).await?.username.history_days;
                    privacy::process_pending(
                        &db,
                        &global,
                        &media,
                        &root,
                        &directory,
                        retention,
                        identity::now(),
                    )
                    .await
                    .map(drop)
                }
            }
        },
    );
    spawn_worker(
        &mut workers,
        &shutdown,
        cache.clone(),
        "retention cleanup",
        SWEEP_PERIOD,
        {
            let db = db.clone();
            let global = global.clone();
            move || {
                let db = db.clone();
                let maintenance = maintenance.clone();
                let global = global.clone();
                async move {
                    // The retention period is an admin setting, read from the in-process cache once per round.
                    let retention = settings(&global, &db).await?.maintenance.clone();
                    maintenance
                        .lock()
                        .await
                        .run(&db, &retention, identity::now())
                        .await
                        .map(drop)
                }
            }
        },
    );
    spawn_worker(
        &mut workers,
        &shutdown,
        cache.clone(),
        "online record cleanup",
        SWEEP_PERIOD,
        {
            let identity = identity.clone();
            move || {
                let identity = identity.clone();
                async move { identity.cleanup_online().await }
            }
        },
    );
    // The two jobs share one round and keep their order: splitting them into two tasks would make them run
    // concurrently, and for now there is only a single instance.
    spawn_worker(
        &mut workers,
        &shutdown,
        cache.clone(),
        "expired sanctions and activities",
        DEADLINE_PERIOD,
        {
            let db = db.clone();
            // Settlement-failure backoff is kept across rounds, so it cannot block other due activities.
            let backoff = std::sync::Arc::new(activities::DueBackoff::default());
            move || {
                let db = db.clone();
                let backoff = backoff.clone();
                async move {
                    let sanctions = moderation::expire_sanctions(&db, identity::now())
                        .await
                        .map(drop)
                        .map_err(anyhow::Error::new);
                    let due = activities::process_due_with(&db, identity::now(), 20, &backoff)
                        .await
                        .map(drop);
                    sanctions.and(due)
                }
            }
        },
    );
    // Rank snapshots. Reads and writes MySQL only and runs as usual with Redis off; boards refreshed by another process
    // very recently (less than half an interval ago) are skipped, so multiple processes do not aggregate twice. The
    // refresh interval is the admin setting `settings.ranks.refresh_seconds`, re-read each round: the task wakes at the
    // shortest interval of 10 seconds and skips the round if less than one interval has passed since the last refresh
    // (docs/settings-and-caching.md, "Keys and groups").
    spawn_worker(
        &mut workers,
        &shutdown,
        cache.clone(),
        "leaderboard snapshot",
        RANKS_TICK,
        {
            let db = db.clone();
            let global = global.clone();
            let last = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
            move || {
                let db = db.clone();
                let global = global.clone();
                let last = last.clone();
                async move {
                    let refresh = settings(&global, &db).await?.ranks.refresh_seconds;
                    let now = identity::now();
                    if now.saturating_sub(last.load(std::sync::atomic::Ordering::Relaxed)) < refresh
                    {
                        return Ok(());
                    }
                    last.store(now, std::sync::atomic::Ordering::Relaxed);
                    gono_forum_web::ranks::refresh(&db, now, refresh / 2)
                        .await
                        .map(drop)
                }
            }
        },
    );

    let listener = tokio::net::TcpListener::from_std(listener)?;
    tracing::info!(url = %format!("http://{}", listener.local_addr()?), version = gono_forum_web::VERSION, "Gono Forum started");
    gono_forum_web::systemd::notify("READY=1\nSTATUS=Serving");
    // Serving: an upgrade that switched to this version has succeeded, so the start guard must not roll it back.
    if let Some(outcome) =
        gono_forum_web::updates::confirm(&gono_forum_web::updates::Paths::for_process(&root))
    {
        tracing::info!(from = %outcome.from, to = %outcome.to, "upgrade confirmed");
    }
    axum::serve(
        listener,
        router::build_with_global(config, &root, db, cache, global)
            .into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        // A one-click upgrade that installed a new binary asks for a graceful exit; systemd starts the new version.
        tokio::select! {
            () = signal() => {}
            () = gono_forum_web::updates::restart_requested() => {}
        }
    })
    .await?;
    gono_forum_web::systemd::notify("STOPPING=1");
    // HTTP no longer accepts requests; let background tasks finish their current round, and only stop waiting on
    // timeout. Aborted leases are reclaimed by their own expiry mechanisms and need no compensation here.
    shutdown.cancel();
    if tokio::time::timeout(SHUTDOWN_GRACE, async {
        while workers.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        tracing::warn!("background tasks did not finish within the time limit; exiting anyway");
    }
    Ok(())
}

async fn signal() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { () = interrupt => {}, () = terminate => {} }
}
