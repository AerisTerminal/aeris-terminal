//! Headless cTrader account and symbol inventory. Never starts browser authorization.
use aeris_ctrader_open_api_adapter::{
    accounts::CtraderAccount,
    host::CtraderHost,
    hosted::{CtraderHostedAccess, load_stored_connection},
    session::CtraderSession,
};
use serde_json::json;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    process::ExitCode,
    sync::{Arc, atomic::AtomicBool},
};

fn masked(value: Option<i64>) -> String {
    let digits = value.map_or_else(|| "unknown".to_string(), |number| number.to_string());
    let suffix = digits.chars().rev().take(2).collect::<String>();
    format!("#****{}", suffix.chars().rev().collect::<String>())
}

fn inventory(
    host: CtraderHost,
    access: &mut CtraderHostedAccess,
    connection: &aeris_platform_runtime::hosted_broker::HostedBrokerConnection,
    stop: &Arc<AtomicBool>,
) -> Result<Vec<serde_json::Value>, String> {
    let token = access.access_token(connection, stop, false)?;
    let credentials = access.app_credentials(connection, stop)?;
    let mut session = CtraderSession::open(host, credentials, token, Arc::clone(stop))
        .map_err(|error| error.to_string())?;
    let accounts: Vec<CtraderAccount> = session
        .accounts()
        .iter()
        .filter(|account| account.is_live == (host == CtraderHost::Live))
        .cloned()
        .collect();
    let mut results = Vec::with_capacity(accounts.len());
    for account in accounts {
        let symbols = session
            .symbol_names(&account)
            .map_err(|error| error.to_string())?;
        let mut safe_symbols: Vec<_> = symbols
            .iter()
            .filter(|name| {
                name.len() <= 64
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"/._-".contains(&byte))
            })
            .map(|name| name.replace('/', ""))
            .collect();
        safe_symbols.sort_unstable();
        let mut samples: Vec<String> = safe_symbols
            .iter()
            .filter(|name| name.as_str() == "EURUSD")
            .take(1)
            .cloned()
            .collect();
        samples.extend(
            safe_symbols
                .into_iter()
                .filter(|name| name != "EURUSD")
                .take(7),
        );
        let environment = if account.is_live { "live" } else { "demo" };
        let login = masked(account.trader_login);
        println!(
            "{environment} account {login}: {} symbols, samples {samples:?}",
            symbols.len()
        );
        results.push(json!({
            "environment": environment,
            "login": login,
            "symbol_count": symbols.len(),
            "sample_symbols": samples
        }));
    }
    session.close();
    Ok(results)
}

fn run() -> Result<(), String> {
    let connection = load_stored_connection()?.ok_or_else(|| {
        "No stored cTrader connection; connect with the human authorization flow".to_string()
    })?;
    let stop = Arc::new(AtomicBool::new(false));
    let mut access = CtraderHostedAccess::new();
    let mut accounts = inventory(CtraderHost::Demo, &mut access, &connection, &stop)?;
    let demo = accounts.len();
    accounts.extend(inventory(
        CtraderHost::Live,
        &mut access,
        &connection,
        &stop,
    )?);
    println!(
        "Demo accounts: {demo}; live accounts: {}",
        accounts.len() - demo
    );
    let directory = Path::new(".cache/evidence");
    fs::create_dir_all(directory).map_err(|_| "Could not create evidence directory")?;
    let sequence = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "System clock is invalid")?
        .as_nanos();
    let evidence = directory.join(format!("ctrader_probe_{sequence}.json"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&evidence)
        .map_err(|_| "Could not create probe evidence")?;
    file.write_all(
        &serde_json::to_vec_pretty(&json!({
            "demo_accounts": demo,
            "live_accounts": accounts.len() - demo,
            "accounts": accounts
        }))
        .map_err(|_| "Could not encode probe evidence")?,
    )
    .map_err(|_| "Could not write probe evidence")?;
    println!("Evidence: {}", evidence.display());
    Ok(())
}

fn main() -> ExitCode {
    if let Err(error) = run() {
        eprintln!("{error}");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
