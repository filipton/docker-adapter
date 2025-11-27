use std::sync::Arc;

use anyhow::Result;
use axum::{Json, Router, extract::State, response::IntoResponse, routing::post};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use serde::{Deserialize, Serialize};
use tokio::{sync::RwLock, time::Instant};

#[derive(Debug)]
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
    let state = Arc::new(AppState {
        handles: Arc::new(RwLock::new(Vec::new())),
        daemon: ServiceDaemon::new()?,
    });

    let app = Router::new()
        .route("/", post(register_mdns))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();

    Ok(())
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
    }

    todo!()
}
