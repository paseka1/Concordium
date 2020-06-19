#![recursion_limit = "1024"]

// Force the system allocator on every platform
use std::alloc::System;
#[global_allocator]
static A: System = System;

use failure::Error;
use p2p_client::{
    common::PeerType,
    p2p::maintenance::{attempt_bootstrap, spawn, P2PNode},
    stats_export_service::instantiate_stats_export_engine,
    utils::get_config_and_logging_setup,
};

use std::{env, process::Command, thread, time::Duration};

fn main() -> Result<(), Error> {
    let (mut conf, app_prefs) = get_config_and_logging_setup()?;
    let data_dir_path = app_prefs.get_user_app_dir();

    conf.connection.max_allowed_nodes = Some(0);
    conf.connection.thread_pool_size = 1;
    conf.connection.dnssec_disabled = true;
    let pager_duty_token = env::var("PD_TOKEN")?;
    let pager_duty_email = env::var("PD_EMAIL")?;
    let pager_duty_svcid = env::var("PD_SVCID")?;

    let stats_export_service = instantiate_stats_export_engine(&conf)?;

    let (node, poll) = P2PNode::new(
        conf.common.id.clone(),
        &conf,
        PeerType::Node,
        stats_export_service,
        Some(data_dir_path),
    );

    spawn(&node, poll, None);

    if !conf.connection.no_bootstrap_dns {
        attempt_bootstrap(&node);
    }

    loop {
        thread::sleep(Duration::from_millis(500));
        if !(node.connections().read().unwrap()).is_empty() {
            println!("Success - I was able to bootstrap");
            return node.close_and_join();
        }
        if node.get_uptime() > 10_000 {
            Command::new("curl")
                .arg("-i")
                .arg("-X")
                .arg("POST")
                .arg("--header")
                .arg("Content-Type: application/json")
                .arg("--header")
                .arg("Accept: application/vnd.pagerduty+json;version=2")
                .arg("--header")
                .arg(&format!("From: {}", pager_duty_email))
                .arg("--header")
                .arg(&format!("Authorization: Token token={}", pager_duty_token))
                .arg("-d")
                .arg(&format!(
                    "
                    {{
                      \"incident\": {{
                        \"type\": \"incident\",
                        \"title\": \"Bootstrapping failed!\",
                        \"service\": {{
                          \"id\": \"{}\",
                          \"type\": \"service_reference\"
                        }}
                      }}
                    }}
                ",
                    pager_duty_svcid
                ))
                .arg("https://api.pagerduty.com/incidents")
                .output()
                .expect("Couldn't send a bootstrap error notification");
            println!("Error - I was not able to bootstrap");
            return node.close_and_join();
        }
    }
}