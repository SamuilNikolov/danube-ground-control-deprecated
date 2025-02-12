// src/main.rs

// Add Rocket’s JSON support via its built-in serde integration.
#[macro_use] extern crate rocket;
use rocket::response::content::RawHtml;
use rocket::serde::{json::Json, Deserialize, Serialize};
use rocket::State;
use std::io::{BufRead, BufReader};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// We'll use the serialport crate to read from the serial port.
use serialport;

// This structure holds the telemetry data.
// (It is marked Serialize/Deserialize for use with Rocket's JSON support.)
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(crate = "rocket::serde")]
struct Telemetry {
    timestamp: u64,
    armed: bool,
    battery: f32,
    arming: f32,
    /// For simplicity we keep the solenoid states as a vector of booleans.
    solenoids: Vec<bool>, // expected length: 16
}

impl Default for Telemetry {
    fn default() -> Self {
        Telemetry {
            timestamp: 0,
            armed: false,
            battery: 0.0,
            arming: 0.0,
            solenoids: vec![false; 16],
        }
    }
}

// Define a shared type alias for our telemetry state.
type SharedTelemetry = Arc<Mutex<Telemetry>>;

/// GET /telemetry returns the current telemetry as JSON.
#[get("/telemetry")]
fn get_telemetry(state: &State<SharedTelemetry>) -> Json<Telemetry> {
    // Lock the shared telemetry and clone it for the response.
    let telemetry = state.lock().unwrap().clone();
    Json(telemetry)
}

/// GET / returns a minimal HTML page that uses JavaScript to poll the telemetry endpoint.
#[get("/")]
fn index() -> RawHtml<&'static str> {
    RawHtml(
    r#"<!DOCTYPE html>
<html>
  <head>
    <meta charset="utf-8">
    <title>Telemetry</title>
  </head>
  <body>
    <h1>Telemetry Visualizer</h1>
    <pre id="telemetry"></pre>
    <script>
      async function fetchTelemetry() {
        try {
          const response = await fetch('/telemetry');
          const data = await response.json();
          document.getElementById('telemetry').innerText = JSON.stringify(data, null, 2);
        } catch (err) {
          console.error(err);
        }
      }
      setInterval(fetchTelemetry, 100);
      fetchTelemetry();
    </script>
  </body>
</html>
"# )
}

/// Given a telemetry line string, try to parse it and return a Telemetry instance.
/// Expected format:
/// TS:<timestamp> | ARM:<0|1> | BATT:<voltage>V | ARM_SENSE:<voltage>V | SOL:ch1:ON, ch2:OFF, …, ch16:OFF
fn parse_telemetry_line(line: &str) -> Option<Telemetry> {
    // Split the line into its parts.
    let parts: Vec<&str> = line.split(" | ").collect();
    if parts.len() != 5 {
        return None;
    }

    // Parse the timestamp.
    let ts_part = parts[0].strip_prefix("TS:")?;
    let timestamp: u64 = ts_part.parse().ok()?;

    // Parse the armed flag.
    let arm_part = parts[1].strip_prefix("ARM:")?;
    let armed = match arm_part {
        "1" => true,
        "0" => false,
        _ => return None,
    };

    // Parse the battery voltage (remove trailing "V").
    let batt_part = parts[2].strip_prefix("BATT:")?;
    let batt_value_str = batt_part.strip_suffix("V")?;
    let battery: f32 = batt_value_str.parse().ok()?;

    // Parse the arming sense voltage.
    let arming_part = parts[3].strip_prefix("ARM_SENSE:")?;
    let arming_value_str = arming_part.strip_suffix("V")?;
    let arming: f32 = arming_value_str.parse().ok()?;

    // Parse solenoid states.
    let sol_part = parts[4].strip_prefix("SOL:")?;
    let sol_entries: Vec<&str> = sol_part.split(',').collect();
    if sol_entries.len() != 16 {
        return None;
    }
    let mut solenoids = Vec::with_capacity(16);
    for entry in sol_entries {
        // Each entry should be in the form "channel:STATE"
        let subparts: Vec<&str> = entry.split(':').collect();
        if subparts.len() != 2 {
            return None;
        }
        let state = match subparts[1].trim() {
            "ON" => true,
            "OFF" => false,
            _ => return None,
        };
        solenoids.push(state);
    }

    Some(Telemetry {
        timestamp,
        armed,
        battery,
        arming,
        solenoids,
    })
}

/// This function spawns a loop that continuously reads lines from the serial port.
/// Each valid telemetry line updates the shared telemetry state.
fn spawn_serial_reader(state: SharedTelemetry) {
    // Use the environment variable "SERIAL_PORT" or default to "/dev/ttyUSB0"
    let port_name = std::env::var("SERIAL_PORT").unwrap_or_else(|_| "/dev/ttyUSB0".into());

    // Open the serial port with the expected baud rate (115200 in this case).
    let port = serialport::new(port_name, 115200)
        .timeout(Duration::from_millis(100))
        .open();

    let mut port = match port {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Failed to open serial port: {:?}", e);
            return;
        }
    };

    // Wrap the serial port in a buffered reader so we can read line‐by‐line.
    let mut reader = BufReader::new(port);

    loop {
        let mut line = String::new();
        // Try to read one line.
        match reader.read_line(&mut line) {
            Ok(n) if n > 0 => {
                if let Some(new_telemetry) = parse_telemetry_line(line.trim()) {
                    // Update the shared telemetry.
                    if let Ok(mut telemetry) = state.lock() {
                        *telemetry = new_telemetry;
                    }
                }
            }
            _ => {
                // No data; sleep briefly to avoid busy‐looping.
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

/// The Rocket entry-point.
#[launch]
fn rocket() -> _ {
    // Create the shared telemetry state.
    let telemetry: SharedTelemetry = Arc::new(Mutex::new(Telemetry::default()));

    // Clone the shared state for the serial reader thread.
    let telemetry_clone = telemetry.clone();
    thread::spawn(move || {
        spawn_serial_reader(telemetry_clone);
    });

    // Build and launch Rocket.
    rocket::build()
        .manage(telemetry)
        .mount("/", routes![index, get_telemetry])
}
