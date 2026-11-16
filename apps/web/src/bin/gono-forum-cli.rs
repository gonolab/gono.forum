use anyhow::{Result, bail};
use gono_forum_web::{config::Config, content, db::Database, i18n::Lang, instance_root};

#[cfg(target_env = "musl")]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

enum Command {
    Migrate,
    SeedDemo,
    SeedSample {
        language: Lang,
    },
    ExportSample {
        dir: std::path::PathBuf,
        language: Lang,
    },
    CheckContent {
        dir: std::path::PathBuf,
    },
    ImportContent {
        dir: std::path::PathBuf,
        members_out: Option<std::path::PathBuf>,
    },
    ImportProfiles {
        fixture_path: std::path::PathBuf,
        confirm_database: String,
    },
    EnableApiKeys {
        fixture_path: std::path::PathBuf,
        output_path: std::path::PathBuf,
    },
    EnableInteractions {
        fixture_path: std::path::PathBuf,
    },
    SetPassword {
        uid: i64,
    },
    RunCampaignPlan {
        founder_uid: i64,
        plan_path: std::path::PathBuf,
    },
    SettleDue {
        limit: i64,
    },
    ResetTwoFactor {
        uid: i64,
        reason: String,
    },
    RekeyTwoFactor,
    SetSiteUrl {
        url: String,
    },
    PurgeNonImageAttachments {
        confirm: bool,
    },
    SkinCheck {
        path: std::path::PathBuf,
        builtin: bool,
    },
    SkinPack {
        dir: std::path::PathBuf,
        output: Option<std::path::PathBuf>,
    },
}

/// Problems listed per kind by check-content and import-content; the rest are counted.
const REPORT_LIMIT: usize = 200;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--version"] || args == ["-V"] {
        println!("gono-forum-cli {}", gono_forum_web::VERSION);
        return Ok(());
    }
    if args.is_empty() || args == ["--help"] || args == ["-h"] {
        println!(
            "Gono Forum database tool\n\n  gono-forum-cli migrate                       Run migrations; creates the SQLite database file if it does not exist (creates no forums: on a fresh install the administrator creates them at /manage/forums)\n  gono-forum-cli seed-demo                     Run migrations, create the fixture's six forums when the forum table is empty, and import the development fixture into an empty content database\n  gono-forum-cli seed-sample [--language=<tag>]  Add the optional sample content offered by the setup wizard (10 members, 5 forums, 100 threads and 1,000 replies; in English unless --language names another language that has a sample, such as zh-CN) to a site that has no forums, threads or posts yet\n  gono-forum-cli export-sample <dir> [--language=<tag>]  Write the same sample content as a content dataset in a new directory, as a starting point for content of your own (docs/content-import.md)\n  gono-forum-cli check-content <dir>           Check a content dataset and list every problem with its file, line and thread key; needs no database or configuration\n  gono-forum-cli import-content <dir> [--members-out=<file>]  Check a content dataset, then import its boards, members, threads and replies into a site that has no forums, threads or posts yet; --members-out writes the new members' uids as a profile list for enable-api-keys and enable-interactions\n  gono-forum-cli import-profiles <path> --confirm-reset=<database>  Bulk-import accounts into the member table from a JSON profile list (empties the member table first; only for test environments that may be reset; the database name must match the currently connected database)\n  gono-forum-cli enable-api-keys <list> <output>  Issue API tokens in bulk for every uid in the profile list; the raw tokens are written to a newly created local output file (mode 0600, refused if it already exists; must not be committed to the repository)\n  gono-forum-cli enable-interactions <list>    Give accounts other than uid=1 that have no e-mail yet a placeholder e-mail and verified status, meeting the e-mail requirement of the like/favorite/reply endpoints\n  gono-forum-cli set-password <uid>            Set a uid's password to the plaintext read from standard input (not echoed, not left in the command-line arguments)\n  gono-forum-cli run-campaign-plan <admin-uid> <plan.json>  Following a plan, in bulk: grant credits / create bounty, red packet, lottery, poll and news campaigns / cast poll votes / accept bounty replies\n  gono-forum-cli settle-due <limit>            Settle due lotteries/bounties/red packets (lottery draws, etc.)\n  gono-forum-cli reset-two-factor <uid> --reason=<reason>  After verifying the member in person offline, turn off their two-factor authentication: delete the authenticator and recovery codes, sign out all devices, revoke API tokens and feed URLs, and send the member a security notice\n  gono-forum-cli rekey-two-factor              After rotating the [identity.two_factor] key, re-encrypt authenticator secrets and lottery redemption codes still encrypted with an old key under the current key; remove the old key only after both report 0 old-key items\n  gono-forum-cli set-site-url <forum-url>      Change the forum URL directly (may include a path, e.g. https://demo.com/bbs; \"\" leaves it empty); for recovery when the admin area is unreachable, and before the first start of a new sub-path install; validated like the admin area, without the second confirmation\n  gono-forum-cli purge-non-image-attachments [--confirm]  List ready non-image attachments (only images are allowed since 2026-11-08); only with --confirm are the post references removed and the attachments marked for cleanup; the objects are deleted by the running Web process, irreversibly\n  gono-forum-cli skin check <dir|file> [--builtin]  Check a skin directory or skin pack exactly as an import does and list every problem and warning (docs/skins.md); --builtin applies the rules of built-in skins; needs no database or configuration\n  gono-forum-cli skin pack <dir> [--output=<file>]  Check a skin directory and write it as one skin pack file for import in the admin area (default <id>-<version>.skin.json in the current directory; refused if the file exists)\n\nConfiguration: GONOF_HOME sets the instance directory (release packages default to the directory holding this binary); GONOF_CONFIG selects the config file (default config/gono-forum.toml in release packages, config/development.toml in source builds); GONOF_DATABASE_URL overrides the database connection."
        );
        return Ok(());
    }
    let command = match args[0].as_str() {
        "migrate" if args.len() == 1 => Command::Migrate,
        "seed-demo" if args.len() == 1 => Command::SeedDemo,
        "seed-sample" if args.len() <= 2 => Command::SeedSample {
            language: sample_language("seed-sample", args.get(1))?,
        },
        "export-sample" if args.len() == 2 || args.len() == 3 => Command::ExportSample {
            dir: std::path::PathBuf::from(&args[1]),
            language: sample_language("export-sample", args.get(2))?,
        },
        "check-content" if args.len() == 2 => Command::CheckContent {
            dir: std::path::PathBuf::from(&args[1]),
        },
        "import-content" if args.len() == 2 || args.len() == 3 => Command::ImportContent {
            dir: std::path::PathBuf::from(&args[1]),
            members_out: match args.get(2) {
                None => None,
                Some(option) => Some(std::path::PathBuf::from(
                    option
                        .strip_prefix("--members-out=")
                        .filter(|path| !path.is_empty())
                        .ok_or_else(|| {
                            anyhow::anyhow!("import-content takes only --members-out=<file> after the directory")
                        })?,
                )),
            },
        },
        "import-profiles" if args.len() == 3 => Command::ImportProfiles {
            fixture_path: std::path::PathBuf::from(&args[1]),
            confirm_database: args[2]
                .strip_prefix("--confirm-reset=")
                .filter(|name| !name.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("import-profiles empties the member table; --confirm-reset=<database> is required")
                })?
                .to_owned(),
        },
        "enable-api-keys" if args.len() == 3 => Command::EnableApiKeys {
            fixture_path: std::path::PathBuf::from(&args[1]),
            output_path: std::path::PathBuf::from(&args[2]),
        },
        "enable-interactions" if args.len() == 2 => Command::EnableInteractions {
            fixture_path: std::path::PathBuf::from(&args[1]),
        },
        "set-password" if args.len() == 2 => Command::SetPassword {
            uid: args[1]
                .parse()
                .map_err(|_| anyhow::anyhow!("uid must be a positive integer"))?,
        },
        "run-campaign-plan" if args.len() == 3 => Command::RunCampaignPlan {
            founder_uid: args[1]
                .parse()
                .map_err(|_| anyhow::anyhow!("administrator uid must be a positive integer"))?,
            plan_path: std::path::PathBuf::from(&args[2]),
        },
        "settle-due" if args.len() == 2 => Command::SettleDue {
            limit: args[1]
                .parse()
                .map_err(|_| anyhow::anyhow!("limit must be a positive integer"))?,
        },
        "reset-two-factor" if args.len() == 3 => Command::ResetTwoFactor {
            uid: args[1]
                .parse()
                .ok()
                .filter(|uid| *uid > 0)
                .ok_or_else(|| anyhow::anyhow!("uid must be a positive integer"))?,
            reason: args[2]
                .strip_prefix("--reason=")
                .filter(|reason| !reason.trim().is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("reset-two-factor requires --reason=<reason>; the reason is written to the audit log")
                })?
                .to_owned(),
        },
        "rekey-two-factor" if args.len() == 1 => Command::RekeyTwoFactor,
        "set-site-url" if args.len() == 2 => Command::SetSiteUrl {
            url: args[1].trim().to_owned(),
        },
        "purge-non-image-attachments" if args.len() == 1 => {
            Command::PurgeNonImageAttachments { confirm: false }
        }
        "purge-non-image-attachments" if args.len() == 2 && args[1] == "--confirm" => {
            Command::PurgeNonImageAttachments { confirm: true }
        }
        "skin" if args.get(1).map(String::as_str) == Some("check")
            && (args.len() == 3 || (args.len() == 4 && args[3] == "--builtin")) =>
        {
            Command::SkinCheck {
                path: std::path::PathBuf::from(&args[2]),
                builtin: args.len() == 4,
            }
        }
        "skin" if args.get(1).map(String::as_str) == Some("pack") && (3..=4).contains(&args.len()) => {
            Command::SkinPack {
                dir: std::path::PathBuf::from(&args[2]),
                output: match args.get(3) {
                    None => None,
                    Some(option) => Some(std::path::PathBuf::from(
                        option
                            .strip_prefix("--output=")
                            .filter(|path| !path.is_empty())
                            .ok_or_else(|| anyhow::anyhow!("skin pack takes only --output=<file> after the directory"))?,
                    )),
                },
            }
        }
        "migrate"
        | "seed-demo"
        | "seed-sample"
        | "export-sample"
        | "check-content"
        | "import-content"
        | "import-profiles"
        | "enable-api-keys"
        | "enable-interactions"
        | "set-password"
        | "run-campaign-plan"
        | "settle-due"
        | "reset-two-factor"
        | "rekey-two-factor"
        | "set-site-url"
        | "purge-non-image-attachments"
        | "skin" => {
            bail!("{}: wrong number of arguments; run gono-forum-cli --help", args[0]);
        }
        _ => bail!("unknown command; run gono-forum-cli --help"),
    };
    gono_forum_web::install_crypto_provider();
    // These two work on files only: whoever prepares a dataset can run them without an installed forum.
    match &command {
        Command::ExportSample { dir, language } => {
            let threads = content::dataset::write_sample(dir, *language)?;
            println!(
                "Wrote the sample content ({language}) to {}: {} members, {} boards, {threads} threads and {} replies. Edit it or use it as a model, check it with gono-forum-cli check-content and import it with gono-forum-cli import-content.",
                dir.display(),
                content::sample::MEMBERS,
                content::sample::FORUMS,
                content::sample::REPLIES
            );
            return Ok(());
        }
        Command::SkinCheck { path, builtin } => {
            let source = if *builtin {
                gono_forum_web::skins::Source::Builtin
            } else {
                gono_forum_web::skins::Source::Imported
            };
            let (_, checked) = gono_forum_web::skins::check_path(path, source);
            print!("{}", gono_forum_web::skins::report(&checked));
            if checked.skin.is_none() {
                bail!("the skin has errors; fix them and check again");
            }
            println!("The skin passes the checks.");
            return Ok(());
        }
        Command::SkinPack { dir, output } => {
            if !dir.is_dir() {
                bail!("{} is not a skin directory", dir.display());
            }
            let (draft, checked) =
                gono_forum_web::skins::check_path(dir, gono_forum_web::skins::Source::Imported);
            print!("{}", gono_forum_web::skins::report(&checked));
            let (Some(draft), Some(_)) = (draft, &checked.skin) else {
                bail!("the skin has errors, so no pack was written; fix them and check again");
            };
            let path = output.clone().unwrap_or_else(|| {
                std::path::PathBuf::from(format!(
                    "{}-{}.skin.json",
                    draft.manifest.id, draft.manifest.version
                ))
            });
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|error| anyhow::anyhow!("cannot create {}: {error}", path.display()))?;
            std::io::Write::write_all(&mut file, gono_forum_web::skins::pack(&draft).as_bytes())?;
            println!(
                "Wrote {}; import it at Appearance > Skins (/manage/skins).",
                path.display()
            );
            return Ok(());
        }
        Command::CheckContent { dir } => {
            let checked = content::dataset::check(dir);
            print!("{}: {}", dir.display(), checked.report.render(REPORT_LIMIT));
            if checked.dataset.is_none() {
                bail!("the content has errors; fix them and check again");
            }
            println!("\nThe content passes the checks.");
            return Ok(());
        }
        _ => {}
    }
    // An import checks the whole dataset before it connects to anything.
    let dataset = match &command {
        Command::ImportContent { dir, members_out } => {
            if let Some(path) = members_out
                && path.exists()
            {
                bail!(
                    "{} already exists; choose another path or delete it first",
                    path.display()
                );
            }
            let checked = content::dataset::check(dir);
            print!("{}: {}", dir.display(), checked.report.render(REPORT_LIMIT));
            let Some(dataset) = checked.dataset else {
                bail!(
                    "the content has errors, so nothing was imported; fix them and run gono-forum-cli check-content again"
                );
            };
            Some(dataset)
        }
        _ => None,
    };
    let root = instance_root()?;
    let override_path = std::env::var_os("GONOF_CONFIG").map(std::path::PathBuf::from);
    let config = Config::load(&root, override_path.as_deref())?;
    // Only migrate creates the SQLite database file; the other commands, like the Web server, require the file to
    // exist.
    let db = if matches!(command, Command::Migrate) {
        Database::connect_creating(&config.database).await?
    } else {
        Database::connect(&config.database).await?
    };
    db.migrate().await?;
    match command {
        Command::Migrate => println!("Database migration complete."),
        Command::SeedDemo => {
            content::seed_fixture(&db).await?;
            println!("Development fixture imported; pages now show it via database queries.");
        }
        Command::ExportSample { .. }
        | Command::CheckContent { .. }
        | Command::SkinCheck { .. }
        | Command::SkinPack { .. } => {
            unreachable!("handled before connecting to the database")
        }
        Command::ImportContent { members_out, .. } => {
            let dataset = dataset.expect("checked before connecting to the database");
            println!("\nImporting...");
            let members = content::import_dataset(
                &db,
                &dataset,
                gono_forum_web::identity::now(),
                |done, total| {
                    if done % 1000 == 0 || done == total {
                        println!("  {done} of {total} threads");
                    }
                },
            )
            .await?;
            if let Some(path) = members_out {
                let mut output = content::profiles::create_secret_file(&path)?;
                let roles: Vec<serde_json::Value> = members
                    .iter()
                    .map(|member| {
                        serde_json::json!({
                            "uid": member.uid,
                            "username": member.username,
                            "bio": member.bio,
                            "regdate": member.regdate,
                        })
                    })
                    .collect();
                std::io::Write::write_all(
                    &mut output,
                    (serde_json::to_string_pretty(&serde_json::json!({ "roles": roles }))? + "\n")
                        .as_bytes(),
                )?;
                println!("Wrote the new members' uids to {}.", path.display());
            }
            println!(
                "Content imported. Restart the forum (or wait for its caches to expire) to see the new boards."
            );
        }
        Command::SeedSample { language } => {
            content::import_sample(&db, gono_forum_web::identity::now(), language).await?;
            println!(
                "Sample content ({language}) added: {} members, {} forums, {} threads and {} replies. Restart the forum (or wait for its caches to expire) to see the new forums.",
                content::sample::MEMBERS,
                content::sample::FORUMS,
                content::sample::THREADS,
                content::sample::REPLIES
            );
        }
        Command::ImportProfiles {
            fixture_path,
            confirm_database,
        } => {
            let summary =
                content::profiles::import_member_profiles(&db, &fixture_path, &confirm_database)
                    .await?;
            println!("Imported profiles: {} members added.", summary.inserted);
        }
        Command::EnableApiKeys {
            fixture_path,
            output_path,
        } => {
            let members = content::profiles::read_uid_username_pairs(&fixture_path)?;
            let uids: Vec<i64> = members.iter().map(|(uid, _)| *uid).collect();
            // Create the output file before issuing: if tokens written to the database could not be saved to disk,
            // their raw values would be lost for good.
            let mut output = content::profiles::create_secret_file(&output_path)?;
            // The lifetime comes from the admin setting `settings.api.key_ttl_days` (read straight from the
            // database; the CLI has no process cache).
            let ttl = gono_forum_web::runtime::read(&db)
                .await
                .map_err(|error| anyhow::anyhow!("failed to read site settings: {error}"))?
                .api
                .key_ttl_days
                .saturating_mul(86_400);
            let keys = match content::api_keys::provision_api_keys(&db, &uids, ttl).await {
                Ok(keys) => keys,
                Err(error) => {
                    drop(output);
                    let _ = std::fs::remove_file(&output_path);
                    return Err(error);
                }
            };
            let usernames: std::collections::HashMap<i64, String> = members.into_iter().collect();
            let entries: Vec<serde_json::Value> = keys
                .iter()
                .map(|key| {
                    serde_json::json!({
                        "uid": key.uid,
                        "username": usernames.get(&key.uid),
                        "api_key": key.secret,
                        "hint": key.hint,
                        "expires_at": key.expires_at,
                    })
                })
                .collect();
            std::io::Write::write_all(
                &mut output,
                (serde_json::to_string_pretty(&entries)? + "\n").as_bytes(),
            )?;
            output.sync_all()?;
            println!(
                "Issued API tokens for {} accounts; the raw tokens were written to {}. Do not commit or share this file.",
                keys.len(),
                output_path.display()
            );
        }
        Command::EnableInteractions { fixture_path } => {
            let updated = content::profiles::enable_interactions(&db, &fixture_path).await?;
            println!("{updated} accounts now have a verified e-mail and can like/favorite/reply.");
        }
        Command::SetPassword { uid } => {
            let password = content::profiles::read_password_line()?;
            content::profiles::set_password(&db, uid, &password).await?;
            println!("Password for uid={uid} updated.");
        }
        Command::RunCampaignPlan {
            founder_uid,
            plan_path,
        } => {
            let summary = content::campaigns::run_plan(&db, founder_uid, &plan_path).await?;
            println!(
                "Campaign plan complete: {} credit grants, {} campaigns created, {} poll votes cast, {} bounties accepted.",
                summary.credits_granted,
                summary.campaigns_published.len(),
                summary.votes_cast,
                summary.bounties_accepted
            );
            if !summary.campaigns_published.is_empty() {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&summary.campaigns_published)?
                );
            }
        }
        Command::SettleDue { limit } => {
            let settled = content::campaigns::settle_due(&db, limit).await?;
            println!("Settled {settled} due campaigns.");
        }
        Command::SetSiteUrl { url } => {
            // Build the handle for the deployment's Redis mode: when enabled, the save is broadcast and every running
            // process drops its settings cache at once.
            let global = gono_forum_web::cache::GlobalCache::new(
                gono_forum_web::cache::Cache::new(&config.redis)?,
            );
            let (before, saved) = gono_forum_web::runtime::set_site_url(&db, &global, &url)
                .await
                .map_err(|error| match error {
                    // Keep internal errors verbatim: the transaction may already be committed and only the
                    // invalidation broadcast failed.
                    gono_forum_web::identity::IdentityError::Internal(cause) => cause,
                    other => anyhow::anyhow!("failed to change the forum URL: {other}"),
                })?;
            let shown = |value: &str| {
                if value.is_empty() {
                    "(empty)".to_owned()
                } else {
                    value.to_owned()
                }
            };
            println!(
                "Forum URL changed from {} to {}.",
                shown(&before),
                shown(&saved.site.url)
            );
            if !config.redis.enabled {
                println!(
                    "Redis is not enabled: running services pick up the change only after the process cache TTL ({} seconds) has passed; to apply it now, restart the services or click \"Search & performance › Refresh cache\" once in the admin area.",
                    saved.cache.ttl_seconds
                );
            }
        }
        Command::ResetTwoFactor { uid, reason } => {
            let identity = identity(&db, &config, &root);
            if identity
                .reset_two_factor(uid, &reason)
                .await
                .map_err(|error| anyhow::anyhow!("two-factor reset failed: {error}"))?
            {
                println!(
                    "Two-factor authentication turned off for uid={uid}: authenticator and recovery codes deleted, all devices signed out, API tokens and feed URLs revoked."
                );
            } else {
                println!("uid={uid} has no two-factor authentication enabled; nothing changed.");
            }
        }
        Command::RekeyTwoFactor => {
            let report = identity(&db, &config, &root)
                .rekey_two_factor()
                .await
                .map_err(|error| anyhow::anyhow!("re-encryption failed: {error}"))?;
            println!(
                "Re-encrypted with the current key: {} rows; already on the current key: {} rows; cannot be decrypted with the configured keys: {} rows.",
                report.rekeyed,
                report.current,
                report.unreadable.len()
            );
            if !report.unreadable.is_empty() {
                println!(
                    "The authenticator secrets of these uids cannot be decrypted (the key that encrypted them is no longer configured); these members can only sign in with a recovery code, or be reset with reset-two-factor: {:?}",
                    report.unreadable
                );
            }
            // Lottery redemption codes share these keys with the authenticator secrets, so move them to the
            // current key as well.
            let codes =
                gono_forum_web::activities::rekey_lottery_codes(&db, &config.identity.two_factor)
                    .await
                    .map_err(|error| anyhow::anyhow!("redemption code rekey failed: {error}"))?;
            println!(
                "Lottery redemption codes: re-encrypted with the current key: {}; already on the current key: {}; cannot be decrypted with the configured keys: {}.",
                codes.rekeyed,
                codes.current,
                codes.unreadable.len()
            );
            if !codes.unreadable.is_empty() {
                println!(
                    "The redemption codes of these (thread, prize, slot) entries cannot be decrypted (the key that encrypted them is no longer configured); neither winners nor authors can see the plaintext, so the author has to correct them on the redemption code page: {:?}",
                    codes.unreadable
                );
            }
        }
        Command::PurgeNonImageAttachments { confirm } => {
            let media =
                gono_forum_web::media::Media::new(db.clone(), config.media.clone(), root.clone());
            let report = media
                .purge_non_image_attachments(confirm)
                .await
                .map_err(|error| anyhow::anyhow!("non-image attachment purge failed: {error}"))?;
            for item in &report.attachments {
                println!(
                    "aid={} uid={} {} ({}, {} bytes) referenced by posts {:?}",
                    item.aid, item.uid, item.filename, item.mime_type, item.filesize, item.pids
                );
            }
            println!(
                "Ready non-image attachments: {}, {} bytes in total, in {} posts.",
                report.attachments.len(),
                report.total_bytes(),
                report.posts()
            );
            if confirm {
                println!(
                    "Unreferenced and queued for cleanup: {}; skipped: {}. The objects are deleted by the cleanup task of the running Web process.",
                    report.queued, report.skipped
                );
            } else if !report.attachments.is_empty() {
                println!(
                    "This was a dry run; nothing changed. To proceed, run gono-forum-cli purge-non-image-attachments --confirm (irreversible)."
                );
            }
        }
    }
    db.pool.close().await;
    Ok(())
}

/// The language of `seed-sample` and `export-sample`: English unless the optional argument is `--language=<tag>`
/// with a language that has a sample.
fn sample_language(command: &str, option: Option<&String>) -> Result<Lang> {
    let Some(option) = option else {
        return Ok(Lang::EN);
    };
    let Some(tag) = option.strip_prefix("--language=") else {
        bail!("{command} takes only --language=<tag> as its last argument");
    };
    content::sample::language(tag).ok_or_else(|| {
        anyhow::anyhow!(
            "there is no sample content in {tag:?}; it exists in {}",
            content::sample::language_tags()
        )
    })
}

fn identity(
    db: &Database,
    config: &Config,
    root: &std::path::Path,
) -> gono_forum_web::identity::Identity {
    // The CLI has no long-running process, so use a process cache handle without a subscription and with Redis
    // disabled: each command builds a fresh one, so it reads the current values in the database.
    let global = gono_forum_web::cache::GlobalCache::new(
        gono_forum_web::cache::Cache::new(&gono_forum_web::config::RedisConfig {
            enabled: false,
            ..gono_forum_web::config::RedisConfig::default()
        })
        .expect("a cache handle with Redis disabled cannot fail"),
    );
    gono_forum_web::identity::Identity::new(
        db.clone(),
        config.identity.clone(),
        root.to_path_buf(),
        global,
    )
}
