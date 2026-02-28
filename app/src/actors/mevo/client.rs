use std::net::SocketAddr;
use std::time::Duration;

use ironsight::conn::Envelope;
use ironsight::protocol::camera::CamConfig;
use ironsight::seq::{self};
use ironsight::{ConnError, Connection};

use super::settings::SessionConfig;
use flighthook::ShotDetectionMode;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Summary of device info collected during handshake.
#[derive(Debug, Clone)]
pub struct HandshakeInfo {
    pub device_info: String,
    pub battery_pct: u8,
    pub external_power: bool,
    pub tilt: f64,
    pub roll: f64,
}

/// Condensed device status from a keepalive poll.
#[derive(Debug, Clone)]
pub struct DeviceStatus {
    pub battery_pct: u8,
    pub external_power: bool,
    pub temp_c: f64,
    pub tilt: f64,
    pub roll: f64,
}

pub struct MevoClient {
    conn: Connection,
}

impl MevoClient {
    pub fn connect(addr: &SocketAddr) -> Result<Self, ConnError> {
        let mut conn = Connection::connect_timeout(addr, CONNECT_TIMEOUT)?;
        conn.set_on_send(|cmd, dest| tracing::debug!(">> {:?} [{}]", cmd, cmd.debug_hex(dest)));
        conn.set_on_recv(|env| tracing::debug!("<< {env:?}"));
        Ok(Self { conn })
    }

    pub fn handshake(&mut self) -> Result<HandshakeInfo, ConnError> {
        let dsp = seq::sync_dsp(&mut self.conn)?;
        let avr = seq::sync_avr(&mut self.conn)?;
        let _pi = seq::sync_pi(&mut self.conn)?;
        let gen_label = dsp.hw_info.device_gen().label();
        Ok(HandshakeInfo {
            device_info: format!("{} ({})", avr.dev_info.text, gen_label),
            battery_pct: dsp.status.battery_percent(),
            external_power: dsp.status.external_power(),
            tilt: avr.status.tilt,
            roll: -avr.status.roll,
        })
    }

    pub fn configure(
        &mut self,
        session_config: &SessionConfig,
        mode: &ShotDetectionMode,
    ) -> Result<(), ConnError> {
        let avr = session_config.to_avr_settings(mode);
        let cam = cam_config();
        seq::configure_avr(&mut self.conn, &avr)?;
        seq::configure_camera(&mut self.conn, &cam)?;
        Ok(())
    }

    pub fn arm(&mut self) -> Result<(), ConnError> {
        seq::arm(&mut self.conn)
    }

    pub fn recv_timeout(&mut self, timeout: Duration) -> Result<Envelope, ConnError> {
        self.conn.recv_timeout(timeout)
    }

    pub fn keepalive(&mut self) -> Result<DeviceStatus, ConnError> {
        let s = seq::keepalive(&mut self.conn)?;
        Ok(DeviceStatus {
            battery_pct: s.dsp.battery_percent(),
            external_power: s.dsp.external_power(),
            temp_c: s.dsp.temperature_c(),
            tilt: s.avr.tilt,
            roll: -s.avr.roll,
        })
    }

    pub fn complete_shot(&mut self) -> Result<(), ConnError> {
        seq::complete_shot(&mut self.conn, |s| tracing::debug!("  {s}"))
    }

    pub fn shutdown(&mut self) {
        self.conn.shutdown().ok();
    }
}

fn cam_config() -> CamConfig {
    CamConfig {
        dynamic_config: true,
        resolution_width: 1024,
        resolution_height: 768,
        rotation: 0,
        ev: 0,
        quality: 80,
        framerate: 20,
        streaming_framerate: 1,
        ringbuffer_pretime_ms: 1000,
        ringbuffer_posttime_ms: 4000,
        raw_camera_mode: 0,
        fusion_camera_mode: false,
        raw_shutter_speed_max: 0.0,
        raw_ev_roi_x: 0,
        raw_ev_roi_y: 0,
        raw_ev_roi_width: 0,
        raw_ev_roi_height: 0,
        raw_x_offset: 0,
        raw_bin44: false,
        raw_live_preview_write_interval_ms: 0,
        raw_y_offset: 0,
        buffer_sub_sampling_pre_trigger_div: 1,
        buffer_sub_sampling_post_trigger_div: 1,
        buffer_sub_sampling_switch_time_offset: 0.0,
        buffer_sub_sampling_total_buffer_size: 0,
        buffer_sub_sampling_pre_trigger_buffer_size: 0,
    }
}
