use anyhow::{Result, anyhow};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post, put},
};
use btleplug::{
    api::{Central as _, Manager as _, Peripheral as _},
    platform::{Adapter, Manager},
};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, str::FromStr, sync::Arc, time::Duration};
use tokio::{sync::RwLock, time::Instant};
use uuid::Uuid;

const HANDLE_INACTIVE_TIMOUET: u128 = 60000;
const DEFAULT_BIND: &str = "0.0.0.0:3127";

#[derive(Debug, Clone)]
pub struct MdnsServiceHandle {
    pub full_name: String,
    pub last_active: Instant,
}

struct AppState {
    pub mdns_handles: Arc<RwLock<Vec<MdnsServiceHandle>>>,
    pub mdns_daemon: ServiceDaemon,
    pub ble_last_scan_devices: Arc<RwLock<Vec<btleplug::platform::Peripheral>>>,
    pub ble_adapter: Option<Adapter>,
}

#[tokio::main]
async fn main() -> Result<()> {
    _ = dotenvy::dotenv();
    let bind = std::env::var("BIND").unwrap_or(DEFAULT_BIND.to_string());
    let bind = SocketAddr::from_str(&bind)?;

    let manager = Manager::new().await?;
    manager.adapters().await?;
    let adapter = manager.adapters().await?.into_iter().next();

    let state = Arc::new(AppState {
        mdns_handles: Arc::new(RwLock::new(Vec::new())),
        mdns_daemon: ServiceDaemon::new()?,
        ble_last_scan_devices: Arc::new(RwLock::new(Vec::new())),
        ble_adapter: adapter,
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
        .route("/mdns", post(register_mdns))
        .route("/ble", post(ble_scan))
        .route("/ble", put(ble_write))
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
            let mut handles = state.mdns_handles.write().await;
            for inactive_handle in handles
                .iter()
                .filter(|h| h.last_active.elapsed().as_millis() >= HANDLE_INACTIVE_TIMOUET)
            {
                println!("[INFO] Unregister: {}", inactive_handle.full_name);
                state.mdns_daemon.unregister(&inactive_handle.full_name)?;
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
        Ok(_) => (StatusCode::OK, "OK".to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("ERROR {e:?}")).into_response(),
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

        let mut handles = state.mdns_handles.write().await;
        let found = handles.iter_mut().find(|x| x.full_name == full_name);
        if let Some(found) = found {
            found.last_active = Instant::now();
        } else {
            handles.push(MdnsServiceHandle {
                full_name,
                last_active: Instant::now(),
            });
            state.mdns_daemon.register(service)?;
        }

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
            &props.service_type.replace("{IF_IP}", &net_ip.to_string()),
            &props.instance_name.replace("{IF_IP}", &net_ip.to_string()),
            &props.host_name.replace("{IF_IP}", &net_ip.to_string()),
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

        let mut handles = state.mdns_handles.write().await;
        let found = handles.iter_mut().find(|x| x.full_name == full_name);
        if let Some(found) = found {
            found.last_active = Instant::now();
        } else {
            println!("[INFO] Register: {}", full_name);
            handles.push(MdnsServiceHandle {
                full_name,
                last_active: Instant::now(),
            });

            state.mdns_daemon.register(service)?;
        }
    }

    Ok(())
}

#[derive(Debug, Serialize)]
pub struct BleAdapterDevice {
    pub device_id: String,
    pub local_name: String,
}

#[derive(Debug, Deserialize)]
pub struct BleScan {
    pub scan_timeout_ms: u64,
}

#[derive(Debug, Deserialize)]
pub struct BleWrite {
    pub device_id: String,
    pub characteristic: Uuid,
    pub data: Vec<u8>,
}

async fn ble_scan(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<BleScan>,
) -> impl IntoResponse {
    match ble_scan_inner(payload, state).await {
        Ok(devices) => Json(devices).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("ERROR {e:?}")).into_response(),
    }
}

async fn ble_scan_inner(data: BleScan, state: Arc<AppState>) -> Result<Vec<BleAdapterDevice>> {
    let Some(ref adapter) = state.ble_adapter else {
        return Err(anyhow!("No adapter found!"));
    };

    let filter = btleplug::api::ScanFilter { services: vec![] };
    adapter.start_scan(filter).await?;

    tokio::time::sleep(Duration::from_millis(data.scan_timeout_ms)).await;

    let mut scan_devices = state.ble_last_scan_devices.write().await;
    scan_devices.clear();

    let mut devices: Vec<BleAdapterDevice> = Vec::new();
    for device in adapter.peripherals().await? {
        let properties = device
            .properties()
            .await?
            .ok_or_else(|| anyhow::anyhow!("No device properties found!"))?;

        let local_name = properties.local_name.unwrap_or("none".to_string());
        devices.push(BleAdapterDevice {
            device_id: device.id().to_string(),
            local_name,
        });
        scan_devices.push(device);
    }

    Ok(devices)
}

async fn ble_write(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<BleWrite>,
) -> impl IntoResponse {
    match ble_write_inner(payload, state).await {
        Ok(_) => (StatusCode::OK, "OK".to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("ERROR {e:?}")).into_response(),
    }
}

async fn ble_write_inner(data: BleWrite, state: Arc<AppState>) -> Result<()> {
    let scan_devices = state.ble_last_scan_devices.read().await;
    let Some(device) = scan_devices
        .iter()
        .find(|x| x.id().to_string() == data.device_id)
    else {
        return Err(anyhow!("Device not found!"));
    };

    if !device.is_connected().await? {
        device.connect().await?;
    }

    device.discover_services().await?;

    let characteristics = device.characteristics();
    let c = characteristics
        .iter()
        .find(|c| c.uuid == data.characteristic)
        .ok_or_else(|| anyhow::anyhow!("Couldn't find characteristic!"))?;

    _ = device
        .write(c, &data.data, btleplug::api::WriteType::WithoutResponse)
        .await;

    _ = device.disconnect().await;
    Ok(())
}
