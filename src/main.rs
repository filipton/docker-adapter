use anyhow::Result;
use axum::{
    Json, Router,
    extract::State,
    response::IntoResponse,
    routing::{get, post},
};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, str::FromStr, sync::Arc, time::Duration};
use tokio::{sync::RwLock, time::Instant};

const HANDLE_INACTIVE_TIMOUET: u128 = 60000;
const DEFAULT_BIND: &str = "0.0.0.0:3127";

#[derive(Debug, Clone)]
pub struct MdnsServiceHandle {
    pub full_name: String,
    pub last_active: Instant,
}

struct AppState {
    pub handles: Arc<RwLock<Vec<MdnsServiceHandle>>>,
    pub daemon: ServiceDaemon,
}

#[tokio::main]
async fn main() -> Result<()> {
    _ = dotenvy::dotenv();
    let bind = std::env::var("BIND").unwrap_or(DEFAULT_BIND.to_string());
    let bind = SocketAddr::from_str(&bind)?;

    let state = Arc::new(AppState {
        handles: Arc::new(RwLock::new(Vec::new())),
        daemon: ServiceDaemon::new()?,
    });

    let state_cloned = state.clone();
    tokio::task::spawn(async move {
        loop {
            let res = unregister_old_task(&state_cloned).await;
            if let Err(e) = res {
                println!("[ERROR] Unregister old handles error: {e:?}");
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
    });

    let app = Router::new()
        .route("/", get(health_check))
        .route("/", post(register_mdns))
        .with_state(state);

    println!("[INFO] Starting listener on: {bind:?}");
    let listener = tokio::net::TcpListener::bind(bind).await.unwrap();
    axum::serve(listener, app).await.unwrap();

    Ok(())
}

async fn health_check() -> impl IntoResponse {
    "OK"
}

async fn unregister_old_task(state: &Arc<AppState>) -> Result<()> {
    loop {
        {
            let mut handles = state.handles.write().await;
            for inactive_handle in handles
                .iter()
                .filter(|h| h.last_active.elapsed().as_millis() >= HANDLE_INACTIVE_TIMOUET)
            {
                println!("[INFO] Unregister: {}", inactive_handle.full_name);
                state.daemon.unregister(&inactive_handle.full_name)?;
            }

            *handles = handles
                .clone()
                .into_iter()
                .filter(|h| h.last_active.elapsed().as_millis() < HANDLE_INACTIVE_TIMOUET)
                .collect();
        }

        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RegisterMdns {
    pub all_interfaces: bool,
    pub properties: Vec<(String, String)>,
    pub service_type: String,
    pub instance_name: String,
    pub ip: Option<String>,
    pub port: u16,
    pub host_name: String,
}
async fn register_mdns(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<RegisterMdns>,
) -> impl IntoResponse {
    match register_mdns_inner(payload, state).await {
        Ok(_) => "OK".to_string(),
        Err(e) => format!("ERROR {e:?}"),
    }
}

async fn register_mdns_inner(props: RegisterMdns, state: Arc<AppState>) -> Result<()> {
    if !props.all_interfaces {
        let Some(ip) = props.ip.clone() else {
            return Ok(());
        };

        let service = ServiceInfo::new(
            &props.service_type,
            &props.instance_name,
            &props.host_name,
            ip,
            props.port,
            props.properties.as_slice(),
        )?;
        let full_name = service.get_fullname().to_string();

        let mut handles = state.handles.write().await;
        let found = handles.iter_mut().find(|x| x.full_name == full_name);
        if let Some(found) = found {
            found.last_active = Instant::now();
        } else {
            handles.push(MdnsServiceHandle {
                full_name,
                last_active: Instant::now(),
            });
        }

        state.daemon.register(service)?;
        return Ok(());
    }

    let network_interfaces = local_ip_address::list_afinet_netifas()?;
    for (_, net_ip) in network_interfaces.iter() {
        if net_ip.is_loopback()
            || net_ip.is_multicast()
            || net_ip.is_unspecified()
            || net_ip.is_ipv6()
        {
            continue;
        }

        let ip = props.ip.clone().unwrap_or(net_ip.to_string());
        let service = ServiceInfo::new(
            &props.service_type,
            &props.instance_name,
            &props.host_name,
            ip,
            props.port,
            props
                .properties
                .clone()
                .into_iter()
                .map(|p| (p.0, p.1.replace("{IF_IP}", &net_ip.to_string())))
                .collect::<Vec<_>>()
                .as_slice(),
        )?;
        let full_name = service.get_fullname().to_string();

        let mut handles = state.handles.write().await;
        let found = handles.iter_mut().find(|x| x.full_name == full_name);
        if let Some(found) = found {
            found.last_active = Instant::now();
        } else {
            println!("[INFO] Register: {}", full_name);
            handles.push(MdnsServiceHandle {
                full_name,
                last_active: Instant::now(),
            });
        }

        state.daemon.register(service)?;
    }

    Ok(())
}
