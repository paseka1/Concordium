extern crate iron;
extern crate router;
extern crate p2p_client;
#[macro_use]
extern crate error_chain;
#[macro_use]
extern crate log;
extern crate env_logger;
extern crate hostname;
#[macro_use]
extern crate serde_json;

use env_logger::Env;
use iron::prelude::*;
use iron::status;
use router::Router;
use p2p_client::errors::*;
use p2p_client::configuration;
use std::sync::{Arc,Mutex};
use std::thread;
use std::net::IpAddr;
use iron::headers::ContentType;
use std::str::FromStr;
use hostname::get_hostname;
use p2p_client::prometheus_exporter::{PrometheusMode,PrometheusServer};
use std::collections::HashSet;

quick_main!( run );

#[derive(Clone)]
struct IpDiscoveryServer {
    prometheus_server: Option<Arc<Mutex<PrometheusServer>>>,
    header_name: String,
    unique_ips: Arc<Mutex<HashSet<String>>>,
}

impl IpDiscoveryServer {
    pub fn new(header_name: String, prometheus: Option<Arc<Mutex<PrometheusServer>>>) -> Self {
        IpDiscoveryServer{
            prometheus_server: prometheus,
            header_name: header_name,
            unique_ips: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    fn index(&self) -> IronResult<Response> {
        if let Some(ref prom) = self.prometheus_server {
            prom.lock().unwrap().pkt_received_inc().map_err(|e| error!("{}", e)).ok();
            prom.lock().unwrap().pkt_sent_inc().map_err(|e| error!("{}", e)).ok();
            prom.lock().unwrap().conn_received_inc().map_err(|e| error!("{}", e)).ok();
        };
        Ok(Response::with((status::Ok, 
        format!("<html><body><h1>IP Discovery service for {} v{}</h1>Operational!</p></body></html>", p2p_client::APPNAME, p2p_client::VERSION
        ))))
    }

    fn get_ip_discovery(&self, req: &mut Request) -> IronResult<Response> {
        let remote_ip = if let Some(ref value) = req.headers.get_raw(&self.header_name) {
            if value.len() == 1 && value[0].len() > 0 {
                match String::from_utf8((*value[0]).to_vec()) {
                    Ok(str_val) => {
                        match IpAddr::from_str(&str_val) {
                            Ok(ip) => ip.to_string(),
                            Err(_) => req.remote_addr.ip().to_string()
                        }
                    }
                    Err(_) => req.remote_addr.ip().to_string(),
                }
            } else {
                req.remote_addr.ip().to_string()
            }
        } else {
            req.remote_addr.ip().to_string()
        };
        if let Some(ref prom) = self.prometheus_server {
            prom.lock().unwrap().pkt_received_inc().map_err(|e| error!("{}", e)).ok();
            prom.lock().unwrap().conn_received_inc().map_err(|e| error!("{}", e)).ok();
            prom.lock().unwrap().pkt_sent_inc().map_err(|e| error!("{}", e)).ok();
            {
                let mut uniques = self.unique_ips.lock().unwrap();
                if uniques.contains(&remote_ip) {
                    uniques.insert(remote_ip.clone());
                    prom.lock().unwrap().unique_ips_inc().map_err(|e| error!("{}", e)).ok();
                }
            }
            
        };
        let return_json = json!({
            "ip": remote_ip,
            "serviceName": "IPDiscovery",
            "serviceVersion": p2p_client::VERSION,
        });
        let mut resp = Response::with((status::Ok,return_json.to_string() ));
        resp.headers.set(ContentType::json());
        Ok(resp)
    }

    pub fn start_server(&mut self, listen_ip: &String, port: u16) -> thread::JoinHandle<()> {
        let mut router = Router::new();
        let _self_clone = Arc::new(self.clone());
        let _self_clone_2 = _self_clone.clone();
        router.get("/", move |_ : &mut Request| _self_clone.clone().index(), "index");
        router.get("/discovery", move| req: &mut Request | _self_clone_2.clone().get_ip_discovery(req), "discovery");
        let _listen = listen_ip.clone();
        thread::spawn(move || {
            Iron::new(router).http(format!("{}:{}", _listen, port)).unwrap();
        })
    }
}

fn run() -> ResultExtWrapper<()>{
    let conf = configuration::parse_ipdiscovery_config();
    let app_prefs = configuration::AppPreferences::new();

    let env = if conf.trace {
        Env::default().filter_or("MY_LOG_LEVEL", "trace")
    } else if conf.debug {
        Env::default().filter_or("MY_LOG_LEVEL", "debug")
    } else {
        Env::default().filter_or("MY_LOG_LEVEL", "info")
    };

     p2p_client::setup_panics();

    env_logger::init_from_env(env);
    info!("Starting up {}-IPDiscovery version {}!",
          p2p_client::APPNAME,
          p2p_client::VERSION);
    info!("Application data directory: {:?}",
          app_prefs.get_user_app_dir());
    info!("Application config directory: {:?}",
          app_prefs.get_user_config_dir());

    let prometheus = if conf.prometheus_server {
        info!("Enabling prometheus server");
        let mut srv = PrometheusServer::new(PrometheusMode::IpDiscoveryMode);
        srv.start_server(&conf.prometheus_listen_addr, conf.prometheus_listen_port).map_err(|e| error!("{}", e)).ok();
        Some(Arc::new(Mutex::new(srv)))
    } else if conf.prometheus_push_gateway.is_some() {
        info!("Enabling prometheus push gateway at {}", &conf.prometheus_push_gateway.clone().unwrap());
        let mut srv = PrometheusServer::new(PrometheusMode::IpDiscoveryMode);
        let instance_name = if let Some(ref instance_id) = conf.prometheus_instance_name {
            instance_id.clone()
        } else {
            match get_hostname() {
                Some(val) => val,
                None => "UNKNOWN-HOST-FIX-ME".to_string()
            }
        };
        srv.start_push_to_gateway( conf.prometheus_push_gateway.unwrap().clone(), conf.prometheus_job_name, instance_name.clone(), 
            conf.prometheus_push_username, conf.prometheus_push_password).map_err(|e| error!("{}", e)).ok();
        Some(Arc::new(Mutex::new(srv)))
    } else {
        None
    };

    let mut ip_discovery = IpDiscoveryServer::new(conf.header_name, prometheus);
    let _th = ip_discovery.start_server(&conf.listen_address, conf.listen_port);
    _th.join().map_err(|e| error!("{:?}", e)).ok();

    Ok(())
}