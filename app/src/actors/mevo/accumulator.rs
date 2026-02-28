use ironsight::Message;
use ironsight::protocol::shot::{ClubResult, FlightResult, FlightResultV1, SpinResult};

use flighthook::{BallFlight, ClubData, Distance, ShotData, SpinData, Velocity};

pub(crate) struct ShotAccumulator {
    flight: Option<FlightResult>,
    flight_v1: Option<FlightResultV1>,
    club: Option<ClubResult>,
    spin: Option<SpinResult>,
    pub(crate) active: bool,
}

impl ShotAccumulator {
    pub(crate) fn new() -> Self {
        Self {
            flight: None,
            flight_v1: None,
            club: None,
            spin: None,
            active: false,
        }
    }

    pub(crate) fn handle(&mut self, msg: &Message) -> bool {
        match msg {
            Message::ShotText(st) if st.is_trigger() => {
                self.reset();
                self.active = true;
                false
            }
            Message::FlightResult(r) => {
                let auto = !self.active;
                self.active = true;
                self.flight = Some(r.clone());
                auto
            }
            Message::FlightResultV1(r) => {
                let auto = !self.active;
                self.active = true;
                self.flight_v1 = Some(r.clone());
                auto
            }
            Message::ClubResult(r) => {
                let auto = !self.active;
                self.active = true;
                self.club = Some(r.clone());
                auto
            }
            Message::SpinResult(r) => {
                let auto = !self.active;
                self.active = true;
                self.spin = Some(r.clone());
                auto
            }
            _ => false,
        }
    }

    pub(crate) fn finalize(&mut self, source: &str) -> Option<ShotData> {
        if !self.active {
            return None;
        }

        // Build ball flight + fallback club data from D4 or E8
        let (shot_number, had_d4, ball, d4_club) = if let Some(d4) = self.flight.take() {
            let ball = BallFlight {
                launch_speed: Velocity::MetersPerSecond(d4.launch_speed),
                launch_elevation: d4.launch_elevation,
                launch_azimuth: d4.launch_azimuth,
                carry_distance: Some(Distance::Meters(d4.carry_distance)),
                total_distance: None, // not populated by DSP
                max_height: Some(Distance::Meters(d4.max_height)),
                flight_time: Some(d4.flight_time),
                roll_distance: None, // not populated by DSP
                backspin_rpm: Some(d4.backspin_rpm),
                sidespin_rpm: Some(d4.sidespin_rpm),
            };
            // D4 embeds club fields — use as fallback when ED is absent
            let club = ClubData {
                club_speed: Velocity::MetersPerSecond(d4.clubhead_speed),
                path: Some(d4.club_strike_direction),
                attack_angle: Some(d4.club_attack_angle),
                face_angle: Some(d4.club_face_angle),
                dynamic_loft: Some(d4.club_effective_loft),
                smash_factor: None, // not in D4
                club_speed_post: Some(Velocity::MetersPerSecond(d4.clubhead_speed_post)),
                swing_plane_horizontal: Some(d4.club_swing_plane_rotation),
                swing_plane_vertical: Some(d4.club_swing_plane_tilt),
                club_offset: None, // not in D4
                club_height: None, // not in D4
            };
            (d4.total, true, ball, Some(club))
        } else if let Some(e8) = self.flight_v1.take() {
            let ball = BallFlight {
                launch_speed: Velocity::MetersPerSecond(e8.ball_velocity),
                launch_elevation: e8.elevation,
                launch_azimuth: e8.azimuth,
                carry_distance: Some(Distance::Meters(e8.distance)),
                total_distance: None, // E8 only has carry, not total
                max_height: Some(Distance::Meters(e8.height)),
                flight_time: Some(e8.flight_time),
                roll_distance: None,
                backspin_rpm: Some(e8.backspin_rpm),
                sidespin_rpm: None, // always zero in E8 — None, not Some(0)
            };
            // E8 has limited club data — speed and path only
            let club = ClubData {
                club_speed: Velocity::MetersPerSecond(e8.club_velocity),
                path: Some(e8.club_strike_direction),
                attack_angle: None,
                face_angle: None,
                dynamic_loft: None,
                smash_factor: None,
                club_speed_post: None,
                swing_plane_horizontal: None,
                swing_plane_vertical: None,
                club_offset: None,
                club_height: None,
            };
            (e8.total, false, ball, Some(club))
        } else {
            self.active = false;
            return None;
        };

        // Club: prefer ED (authoritative), fall back to D4/E8 embedded fields
        let club = if let Some(ed) = self.club.take() {
            Some(club_from_ed(&ed))
        } else {
            d4_club
        };

        let spin = self.spin.take().map(|s| SpinData {
            total_spin: s.pm_spin_final,
            spin_axis: s.spin_axis,
        });

        let shot = ShotData {
            source: source.to_string(),
            shot_number,
            ball,
            club,
            spin,
            estimated: !had_d4,
        };
        self.active = false;
        Some(shot)
    }

    pub(crate) fn has_flight(&self) -> bool {
        self.flight.is_some()
    }

    pub(crate) fn has_flight_v1(&self) -> bool {
        self.flight_v1.is_some()
    }

    pub(crate) fn has_club(&self) -> bool {
        self.club.is_some()
    }

    pub(crate) fn has_spin(&self) -> bool {
        self.spin.is_some()
    }

    fn reset(&mut self) {
        self.flight = None;
        self.flight_v1 = None;
        self.club = None;
        self.spin = None;
        self.active = false;
    }
}

fn club_from_ed(ed: &ClubResult) -> ClubData {
    ClubData {
        club_speed: Velocity::MetersPerSecond(ed.pre_club_speed),
        path: Some(ed.strike_direction),
        attack_angle: Some(ed.attack_angle),
        face_angle: Some(ed.face_angle),
        dynamic_loft: Some(ed.dynamic_loft),
        smash_factor: Some(ed.smash_factor),
        club_speed_post: Some(Velocity::MetersPerSecond(ed.post_club_speed)),
        swing_plane_horizontal: Some(ed.swing_plane_horizontal),
        swing_plane_vertical: Some(ed.swing_plane_vertical),
        club_offset: Some(Distance::Meters(ed.club_offset)),
        club_height: Some(Distance::Meters(ed.club_height)),
    }
}
