use btleplug::api::{Central, Manager as _, Peripheral as _};
use btleplug::platform::{Adapter, Manager, Peripheral, PeripheralId};
use uuid::Uuid;

pub const FLIPPER_TX_UUID: Uuid = Uuid::from_u128(0x19ed82ae_ed21_4c9d_4145_228e62fe0000);

pub const FLIPPER_RX_UUID: Uuid = Uuid::from_u128(0x19ed82ae_ed21_4c9d_4145_228e61fe0000);

pub const FLIPPER_FLOW_UUID: Uuid = Uuid::from_u128(0x19ed82ae_ed21_4c9d_4145_228e63fe0000);

pub async fn get_central(manager: &Manager) -> Adapter {
    manager
        .adapters()
        .await
        .unwrap()
        .into_iter()
        .nth(0)
        .unwrap()
}

pub async fn get_flipper(central: &Adapter, id: &PeripheralId) -> Option<Peripheral> {
    let target_name =
        std::env::var("FLIPPER_DEVICE_NAME").unwrap_or_else(|_| "TYECzer0".to_string());

    for p in central
        .peripherals()
        .await
        .unwrap()
        .iter()
        .filter(|p| p.id() == *id)
    {
        let properties = match p.properties().await {
            Ok(Some(properties)) => properties,

            _ => continue,
        };

        if properties
            .local_name
            .as_ref()
            .map(|name| name == &target_name)
            .unwrap_or(false)
        {
            return Some(p.clone());
        }
    }

    None
}
