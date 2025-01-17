use crate::prod_tests::test_env::TestEnv;
use anyhow::Result;
use chrono::{DateTime, SecondsFormat, Utc};
use ic_nns_init::set_up_env_vars_for_all_canisters;
use rand_chacha::{rand_core, ChaCha8Rng};
use slog::{o, warn, Drain, Logger};
use slog_async::OverflowStrategy;
use std::time::SystemTime;
use std::{
    fs,
    fs::File,
    path::{Path, PathBuf},
    time::Duration,
};
use url::Url;

use super::cli::{AuthorizedSshAccount, ValidatedCliArgs};
use super::farm::Farm;
use super::pot_dsl;

const ASYNC_CHAN_SIZE: usize = 8192;
const DEFAULT_FARM_BASE_URL: &str = "https://farm.dfinity.systems";

pub const FARM_GROUP_NAME: &str = "farm/group_name";
pub const FARM_BASE_URL: &str = "farm/base_url";
pub const BASE_IMG_URL: &str = "base_img_url";
pub const BASE_IMG_SHA256: &str = "base_img_sha256";
pub const INITIAL_REPLICA_VERSION: &str = "initial_replica_version";
pub const JOURNALBEAT_HOSTS: &str = "journalbeat_hosts";
pub const LOG_DEBUG_OVERRIDES: &str = "log_debug_overrides";
pub const AUTHORIZED_SSH_ACCOUNTS: &str = "ssh/authorized_accounts";
pub const AUTHORIZED_SSH_ACCOUNTS_DIR: &str = "ssh/authorized_accounts_dir";
pub const POT_TIMEOUT: &str = "pot_timeout";

pub fn initialize_env(env: &TestEnv, cli_args: &ValidatedCliArgs) -> Result<()> {
    let farm_base_url = cli_args
        .farm_base_url
        .clone()
        .unwrap_or_else(|| Url::parse(DEFAULT_FARM_BASE_URL).expect("should not fail!"));
    env.write_object(FARM_BASE_URL, &farm_base_url)?;
    env.write_object(AUTHORIZED_SSH_ACCOUNTS, &cli_args.authorized_ssh_accounts)?;
    setup_ssh_key_dir(env, &cli_args.authorized_ssh_accounts)?;
    env.write_object(BASE_IMG_URL, &cli_args.base_img_url)?;
    env.write_object(BASE_IMG_SHA256, &cli_args.base_img_sha256)?;
    env.write_object(JOURNALBEAT_HOSTS, &cli_args.journalbeat_hosts)?;
    env.write_object(INITIAL_REPLICA_VERSION, &cli_args.initial_replica_version)?;
    env.write_object(LOG_DEBUG_OVERRIDES, &cli_args.log_debug_overrides)?;
    Ok(())
}

pub fn create_driver_context_from_cli(
    cli_args: ValidatedCliArgs,
    env: TestEnv,
    hostname: Option<String>,
) -> DriverContext {
    let created_at = SystemTime::now();
    let job_id = cli_args.job_id.unwrap_or_else(|| {
        let datetime: DateTime<Utc> = DateTime::from(created_at);
        let job_id = hostname
            .map(|s| format!("{}-", s))
            .unwrap_or_else(|| "".to_string());
        format!(
            "{}{}",
            job_id,
            datetime.to_rfc3339_opts(SecondsFormat::Millis, true)
        )
    });

    let farm_url = cli_args
        .farm_base_url
        .unwrap_or_else(|| Url::parse(DEFAULT_FARM_BASE_URL).expect("should not fail!"));

    let rng = rand_core::SeedableRng::seed_from_u64(cli_args.rand_seed);
    let logger = mk_logger();
    let farm = Farm::new(farm_url, logger.clone());

    // Setting the global env variables that point to the wasm files for NNS
    // canisters. This is a known hack inherited from the canister_test
    // framework.
    if let Some(p) = &cli_args.nns_canister_path {
        set_up_env_vars_for_all_canisters(p);
    } else {
        warn!(
            logger,
            "Path to nns canister not provided; tests might not be able to install them!"
        );
    }

    DriverContext {
        logger: logger.clone(),
        rng,
        created_at,
        job_id,
        farm,
        logs_base_dir: cli_args.log_base_dir,
        pot_timeout: cli_args.pot_timeout,
        env,
        working_dir: cli_args.working_dir,
    }
}

pub fn mk_logger() -> Logger {
    let decorator = slog_term::TermDecorator::new().build();
    let drain = slog_term::FullFormat::new(decorator).build().fuse();
    let drain = slog_async::Async::new(drain)
        .chan_size(ASYNC_CHAN_SIZE)
        .build();
    slog::Logger::root(drain.fuse(), o!())
}

pub fn tee_logger(ctx: &DriverContext, test_path: &pot_dsl::TestPath) -> Logger {
    if let Some(base_dir) = ctx.logs_base_dir.clone() {
        let stdout_drain = slog::LevelFilter::new(ctx.logger.clone(), slog::Level::Warning);
        let file_drain = slog_term::FullFormat::new(slog_term::PlainSyncDecorator::new(
            File::create(set_up_filepath(&base_dir, test_path))
                .expect("could not create a log file"),
        ))
        .build()
        .fuse();
        let file_drain = slog_async::Async::new(file_drain)
            .chan_size(ASYNC_CHAN_SIZE)
            .overflow_strategy(OverflowStrategy::Block)
            .build()
            .fuse();
        slog::Logger::root(slog::Duplicate(stdout_drain, file_drain).fuse(), o!())
    } else {
        ctx.logger.clone()
    }
}

fn set_up_filepath(base_dir: &Path, test_path: &pot_dsl::TestPath) -> PathBuf {
    let mut tp = test_path.clone();
    let filename = tp.pop();
    let mut path = tp.to_filepath(base_dir);
    fs::create_dir_all(&path).unwrap();
    path.push(filename);
    path.set_extension("log");
    path
}

/// Setup a directory containing files as consumed by the bootstrap script.
fn setup_ssh_key_dir(env: &TestEnv, key_pairs: &[AuthorizedSshAccount]) -> Result<()> {
    for key_pair_files in key_pairs {
        env.write_object(
            [AUTHORIZED_SSH_ACCOUNTS_DIR, &key_pair_files.name]
                .iter()
                .collect::<PathBuf>(),
            key_pair_files.public_key.as_slice(),
        )?;
    }
    Ok(())
}

#[derive(Clone)]
pub struct DriverContext {
    /// logger
    pub logger: Logger,
    pub rng: ChaCha8Rng,
    /// The instance at which the context was created.
    pub created_at: SystemTime,
    /// A unique id identifying this test run.
    pub job_id: String,
    /// Abstraction for the Farm service
    pub farm: Farm,
    pub logs_base_dir: Option<PathBuf>,
    pub pot_timeout: Duration,
    pub env: TestEnv,
    pub working_dir: PathBuf,
}

impl DriverContext {
    pub fn logger(&self) -> slog::Logger {
        self.logger.clone()
    }
}
