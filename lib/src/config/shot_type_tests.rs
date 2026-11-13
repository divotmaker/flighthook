//! Tests of the config: shot type.

use super::*;

fn with(edit: impl FnOnce(&mut FlighthookConfig)) -> FlighthookConfig {
    let mut config = FlighthookConfig::default();
    edit(&mut config);
    config
}

/// A Mevo, a Trackball and a Mock launch monitor take the shot type as
/// a hint; one test per card type.
#[test]
fn a_mevo_card_shows_the_shot_type() {
    assert!(with(|c| _ = c.mevo.insert("0".into(), MevoSection::default())).shot_type_available());
}

#[test]
fn a_trackball_card_shows_the_shot_type() {
    assert!(
        with(|c| _ = c.trackball.insert("0".into(), TrackballSection::default()))
            .shot_type_available()
    );
}

#[test]
fn a_mock_launch_monitor_shows_the_shot_type() {
    assert!(
        with(|c| _ = c
            .mock_monitor
            .insert("0".into(), MockMonitorSection::default()))
        .shot_type_available()
    );
}

/// The R10, the Omni, the ExPutt, the Open Connect server and the
/// Pureplay Accessory ignore the hint.
#[test]
fn cards_that_ignore_the_hint_do_not_show_it() {
    let config = with(|c| {
        c.r10.insert("0".into(), R10Section::default());
        c.square.insert("0".into(), SquareSection::default());
        c.exputt.insert("0".into(), ExPuttSection::default());
        c.openconnect_server
            .insert("0".into(), OpenConnectServerSection::default());
        c.pureplay_accessory
            .insert("0".into(), PureplayAccessorySection::default());
    });
    assert!(!config.shot_type_available());
}

#[test]
fn a_card_that_is_off_does_not_count() {
    let mevo = MevoSection {
        enabled: false,
        ..Default::default()
    };
    assert!(!with(|c| _ = c.mevo.insert("0".into(), mevo)).shot_type_available());
}

/// An output card counts only when it routes chips or putts to a
/// specific monitor: "any" and a full-swing monitor alone do not.
#[test]
fn routing_chips_or_putts_to_a_monitor_shows_the_shot_type() {
    let client = |edit: fn(&mut OpenConnectClientSection)| {
        with(|c| {
            let mut s = OpenConnectClientSection::default();
            edit(&mut s);
            c.openconnect_client.insert("0".into(), s);
        })
    };
    assert!(!client(|_| {}).shot_type_available(), "any");
    assert!(
        !client(|s| s.full_monitor = Some("r10.0".into())).shot_type_available(),
        "full only"
    );
    assert!(
        client(|s| s.chipping_monitor = Some("r10.0".into())).shot_type_available(),
        "chip"
    );
    assert!(
        client(|s| s.putting_monitor = Some("exputt.0".into())).shot_type_available(),
        "putt"
    );
    let off = OpenConnectClientSection {
        putting_monitor: Some("exputt.0".into()),
        enabled: false,
        ..Default::default()
    };
    assert!(!with(|c| _ = c.openconnect_client.insert("0".into(), off)).shot_type_available());
    let pureplay = PureplaySection {
        chipping_monitor: Some("r10.0".into()),
        ..Default::default()
    };
    assert!(with(|c| _ = c.pureplay.insert("0".into(), pureplay)).shot_type_available());
}

#[test]
fn a_card_that_sets_the_shot_type_is_named() {
    let id = "openconnect_client.0";
    let mut empty = OpenConnectClientSection::default();
    empty.chipping_clubs.clear();
    empty.putting_clubs.clear();
    assert!(!with(|c| _ = c.openconnect_client.insert("0".into(), empty)).card_sets_shot_type(id));
    let client = with(|c| {
        _ = c
            .openconnect_client
            .insert("0".into(), OpenConnectClientSection::default());
    });
    assert!(client.card_sets_shot_type(id));
    assert!(
        !client.card_sets_shot_type("openconnect_client.1"),
        "no such card"
    );
    let off = OpenConnectClientSection {
        enabled: false,
        ..Default::default()
    };
    assert!(!with(|c| _ = c.openconnect_client.insert("0".into(), off)).card_sets_shot_type(id));
    assert!(
        with(|c| _ = c.pureplay.insert("0".into(), PureplaySection::default()))
            .card_sets_shot_type("pureplay.0")
    );
    assert!(
        with(|c| _ = c
            .pureplay_accessory
            .insert("0".into(), PureplayAccessorySection::default()))
        .card_sets_shot_type("pureplay_accessory.0")
    );
    let mevo = with(|c| _ = c.mevo.insert("0".into(), MevoSection::default()));
    assert!(!mevo.card_sets_shot_type("mevo.0"));
    assert!(!FlighthookConfig::default().card_sets_shot_type(id));
    assert!(!FlighthookConfig::default().card_sets_shot_type("pureplay"));
}
