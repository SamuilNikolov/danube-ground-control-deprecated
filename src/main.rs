// src/main.rs

#[macro_use] extern crate rocket;
use rocket::response::content::RawHtml;
use rocket::serde::{json::Json, Deserialize, Serialize};
use rocket::State;
use std::env;
use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;
use serialport;

/// The telemetry structure matching the Arduino telemetry format.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(crate = "rocket::serde")]
struct Telemetry {
    timestamp: u64,
    armed: bool,
    battery: f32,
    arming: f32,
    /// For simplicity we keep the solenoid states as a vector of booleans (length 16).
    solenoids: Vec<bool>,
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

/// A shared telemetry type.
type SharedTelemetry = Arc<Mutex<Telemetry>>;

/// Our application state now holds both the telemetry and a command sender.
/// When a button is pressed, the corresponding command string (e.g. "a", "d", or "s51")
/// is sent via this channel to the serial loop thread.
struct AppState {
    telemetry: SharedTelemetry,
    command_tx: mpsc::Sender<String>,
}

/// GET /telemetry returns the current telemetry as JSON.
#[get("/telemetry")]
fn get_telemetry(state: &State<AppState>) -> Json<Telemetry> {
    let tel = state.telemetry.lock().unwrap().clone();
    Json(tel)
}

/// POST /arm sends an "arm" command (the Arduino expects "a")
#[post("/arm")]
fn arm(state: &State<AppState>) -> &'static str {
    let _ = state.command_tx.send("a".to_string());
    "OK"
}

/// POST /disarm sends a "disarm" command (the Arduino expects "d")
#[post("/disarm")]
fn disarm(state: &State<AppState>) -> &'static str {
    let _ = state.command_tx.send("d".to_string());
    "OK"
}

/// POST /solenoid/<channel>/<sstate> sends a solenoid actuation command.
/// For example, POST /solenoid/5/1 sends "s51" (channel 5 → state 1).
#[post("/solenoid/<channel>/<sstate>")]
fn solenoid(channel: u8, sstate: u8, state: &State<AppState>) -> &'static str {
    // Validate channel (1..16) and state (0 or 1)
    if channel < 1 || channel > 16 || (sstate != 0 && sstate != 1) {
         return "Invalid parameters";
    }
    let cmd = format!("s{}{}", channel, sstate);
    let _ = state.command_tx.send(cmd);
    "OK"
}

/// GET / serves the main HTML page.
/// The page creates buttons for all 16 solenoids and for arm/disarm,
/// and it polls /telemetry to update the UI.
#[get("/")]
fn index() -> RawHtml<&'static str> {
    RawHtml(r#"<!DOCTYPE html>
<html>
<head>
   <meta charset="utf-8">
   <title>Telemetry Control</title>
   <style>
      .solenoid-button {
         width: 100px;
         height: 40px;
         margin: 5px;
      }
      .on { background-color: green; color: white; }
      .off { background-color: red; color: white; }
   </style>
</head>
<body>
   <h1>Telemetry Control</h1>
   <div>
      <button id="armButton" onclick="sendArm()">Arm</button>
      <button id="disarmButton" onclick="sendDisarm()">Disarm</button>
   </div>
   <h2>Solenoids</h2>
   <div id="solenoids"></div>
   <h2>Raw Telemetry</h2>
   <pre id="telemetry"></pre>
   <script>
      const NUM_SOLENOIDS = 16;
      const solenoidContainer = document.getElementById('solenoids');
      // Dynamically create a button for each solenoid.
      for (let i = 0; i < NUM_SOLENOIDS; i++) {
         const btn = document.createElement('button');
         btn.id = 'solenoid' + (i+1);
         btn.className = 'solenoid-button off';
         btn.innerText = 'Solenoid ' + (i+1) + ': OFF';
         // When clicked, we read the current telemetry and then send a command
         // to toggle the state.
         btn.onclick = () => toggleSolenoid(i);
         solenoidContainer.appendChild(btn);
      }

      async function sendArm() {
         try {
             await fetch('/arm', { method: 'POST' });
         } catch(e) { console.error(e); }
      }
      async function sendDisarm() {
         try {
             await fetch('/disarm', { method: 'POST' });
         } catch(e) { console.error(e); }
      }
      async function toggleSolenoid(index) {
         try {
             const response = await fetch('/telemetry');
             const data = await response.json();
             // Toggle: if currently ON then turn it OFF and vice versa.
             const currentState = data.solenoids[index];
             const newState = currentState ? 0 : 1;
             const channel = index + 1;
             await fetch(`/solenoid/${channel}/${newState}`, { method: 'POST' });
         } catch (err) {
             console.error(err);
         }
      }

      async function fetchTelemetry() {
         try {
            const response = await fetch('/telemetry');
            const data = await response.json();
            document.getElementById('telemetry').innerText = JSON.stringify(data, null, 2);
            // Enable/disable arm/disarm buttons based on telemetry state.
            if (data.armed) {
                document.getElementById('armButton').disabled = true;
                document.getElementById('disarmButton').disabled = false;
            } else {
                document.getElementById('armButton').disabled = false;
                document.getElementById('disarmButton').disabled = true;
            }
            // Update each solenoid button to reflect its actual state.
            for (let i = 0; i < NUM_SOLENOIDS; i++) {
                const btn = document.getElementById('solenoid' + (i+1));
                if (data.solenoids[i]) {
                   btn.classList.add('on');
                   btn.classList.remove('off');
                   btn.innerText = `Solenoid ${i+1}: ON`;
                } else {
                   btn.classList.add('off');
                   btn.classList.remove('on');
                   btn.innerText = `Solenoid ${i+1}: OFF`;
                }
            }
         } catch (err) {
            console.error(err);
         }
      }

      // Poll telemetry frequently.
      setInterval(fetchTelemetry, 100);
      fetchTelemetry();
   </script>
</body>
</html>
"#)
}

/// Given a telemetry line string from the Arduino, parse and return a Telemetry instance.
///
/// Expected format (as sent from your Arduino):
/// TS:<timestamp> | ARM:<0|1> | BATT:<voltage>V | ARM_SENSE:<voltage>V | SOL:1:ON,2:OFF,...,16:OFF
fn parse_telemetry_line(line: &str) -> Option<Telemetry> {
    let parts: Vec<&str> = line.split(" | ").collect();
    if parts.len() != 5 {
        return None;
    }
    // Parse timestamp.
    let ts_part = parts[0].strip_prefix("TS:")?;
    let timestamp: u64 = ts_part.parse().ok()?;
    // Parse armed flag.
    let arm_part = parts[1].strip_prefix("ARM:")?;
    let armed = match arm_part {
        "1" => true,
        "0" => false,
        _ => return None,
    };
    // Parse battery voltage (strip trailing "V").
    let batt_part = parts[2].strip_prefix("BATT:")?;
    let batt_value_str = batt_part.strip_suffix("V")?;
    let battery: f32 = batt_value_str.parse().ok()?;
    // Parse arming sense voltage.
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
        // Each entry should be in the format "channel:ON" or "channel:OFF"
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

/// This thread opens the serial port (using the provided port name), then continuously
/// (a) checks for command strings from the channel and writes them to the port (with a newline)
/// and (b) reads telemetry lines from the Arduino, parses them, and updates the shared telemetry.
fn spawn_serial_loop(telemetry: SharedTelemetry, rx: mpsc::Receiver<String>, port_name: String) {
    let port_result = serialport::new(port_name.clone(), 115200)
        .timeout(Duration::from_millis(100))
        .open();
    let mut port = match port_result {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Failed to open serial port '{}': {:?}", port_name, e);
            return;
        }
    };

    // Clone the port for reading (most serialport implementations allow cloning for read/write).
    let port_clone = match port.try_clone() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Failed to clone serial port: {:?}", e);
            return;
        }
    };
    let mut reader = BufReader::new(port_clone);

    loop {
        // If any commands have been sent (via the Rocket endpoints), write them now.
        while let Ok(cmd) = rx.try_recv() {
            let cmd_with_newline = cmd + "\n";
            if let Err(e) = port.write_all(cmd_with_newline.as_bytes()) {
                eprintln!("Error writing to serial port: {:?}", e);
            }
        }
        // Try to read a line of telemetry.
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(n) if n > 0 => {
                if let Some(new_telemetry) = parse_telemetry_line(line.trim()) {
                    if let Ok(mut tel) = telemetry.lock() {
                        *tel = new_telemetry;
                    }
                }
            },
            _ => {
                // No (or incomplete) data was available.
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// Rocket’s entry point.
/// It reads (or defaults) the serial port name, creates the shared telemetry and
/// command channel, spawns the serial loop thread, and mounts the endpoints.
#[launch]
fn rocket() -> _ {
    // Use the first command-line argument as the port name, defaulting to "COM5" if none is provided.
    let port_name = env::args().nth(1).unwrap_or_else(|| "COM5".into());
    println!("Using serial port: {}", port_name);

    // Shared telemetry state.
    let telemetry: SharedTelemetry = Arc::new(Mutex::new(Telemetry::default()));
    // Create a channel for sending command strings to the serial loop.
    let (tx, rx) = mpsc::channel::<String>();

    // Spawn the serial loop thread.
    let telemetry_clone = telemetry.clone();
    let port_name_clone = port_name.clone();
    thread::spawn(move || {
        spawn_serial_loop(telemetry_clone, rx, port_name_clone);
    });

    // Build the application state and launch Rocket.
    let app_state = AppState { telemetry, command_tx: tx };

    rocket::build()
        .manage(app_state)
        .mount("/", routes![index, get_telemetry, arm, disarm, solenoid])
}
