use heim::{
    sensors::TemperatureSensor,
    units::{thermodynamic_temperature::degree_celsius, ThermodynamicTemperature as Temperature},
};

use slog::{debug, info, o, Logger};

use std::collections::BTreeMap;

/// Properties of a given sensor, within a sensor unit
struct SensorProperties {
    label: Option<String>,
    high_trip_point: Option<Temperature>,
    critical_trip_point: Option<Temperature>,
}

/// Report on the host's sensors
pub fn startup_report(log: &Logger, temperatures: Vec<TemperatureSensor>) {
    // Group sensors by sensor unit
    debug!(log, "Processing temperature sensor list...");
    let mut unit_to_sensors = BTreeMap::<String, Vec<_>>::new();
    for sensor in temperatures {
        let sensor_list = unit_to_sensors.entry(sensor.unit().to_owned()).or_default();
        sensor_list.push(SensorProperties {
            label: sensor.label().map(|label| label.to_owned()),
            high_trip_point: sensor.high(),
            critical_trip_point: sensor.critical(),
        });
    }

    // Report on sensor units and their inner sensors
    for (unit, mut sensor_list) in unit_to_sensors {
        let unit_log = log.new(o!("sensor unit" => unit));
        sensor_list.sort_by_cached_key(|sensor| sensor.label.clone());
        for sensor in sensor_list {
            let to_celsius = |t_opt: Option<Temperature>| t_opt.map(|t| t.get::<degree_celsius>());
            info!(unit_log, "Found a temperature sensor";
                  "label" => sensor.label,
                  "high trip point (°C)" => to_celsius(sensor.high_trip_point),
                  "critical trip point (°C)" => to_celsius(sensor.critical_trip_point));
        }
    }
}
