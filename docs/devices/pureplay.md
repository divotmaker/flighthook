# Pureplay Accessory

**Status: Beta.** Pureplay's Accessory API is early access: Pureplay can
still change it, and a Pureplay update can need a flighthook update.

The Pureplay Accessory card follows a round in
[Pureplay](https://github.com/PureplayGolf/Pureplay-Accessory) through
Pureplay's Accessory API, a WebSocket on port 9770. While it is connected:

- Pureplay owns the club and the hand: the club and hand of the player who
  is up become flighthook's, so a launch monitor that arms for the club
  (putting, chipping, full swing) follows the club picked in Pureplay. With
  **Aim and Club** allowed, a club picked in flighthook's header changes
  Pureplay's club.
- Pureplay is a launch monitor for flighthook: a shot of a player at the
  Pureplay computer becomes a flighthook shot, whatever launch monitor
  Pureplay reads. It shows in the Shots tab, cuts the cameras' clips and is
  exported like any shot.
- Pureplay's result for a shot (holed, green, water, out of bounds, its
  carry and total) is kept on the shot.

The club is the one thing the card changes in Pureplay. flighthook never
changes the aim: it has no aim control.

## Two ways to use it

- **The launch monitor is connected to Pureplay.** flighthook takes the
  shots from the card. Leave Shots on Auto (or pick From Pureplay).
- **The launch monitor is connected to flighthook**, which sends its shots
  to Pureplay through an output: the Open Connect card,
  see [Pureplay](../outputs/pureplay.md). Pureplay reports each of those
  shots back to the card; the card does not take them again, and Pureplay's
  result lands on the launch monitor's shot. Leave Shots on Auto (or pick
  From flighthook).

Auto tells the two apart shot by shot: a shot Pureplay reports within 5
seconds of a shot an output sent to a simulator is that shot coming back.
Auto counts sends to any simulator, so with GSPro and Pureplay both in use
set Shots to From Pureplay or From flighthook. Auto assumes one launch
monitor, on one side: with launch monitors on both sides at once, a shot
from Pureplay's that lands within 5 seconds of a flighthook shot Pureplay
never played is taken for it. Pick From Pureplay or From flighthook to
settle it.

The card never feeds an output: its shots and its readiness never go to
GSPro, Pureplay or any other simulator, whatever the outputs' routing says.
An output's launch monitor pickers do not list it.

## Setup

1. **Allow flighthook in Pureplay.** Open **Settings > Accessories** and set
   **Allow apps from**: **This PC** when flighthook runs on the Pureplay
   computer, **Your network** when it runs on another one. Leave **Ask
   about new apps** on. Press Apply or Save.
2. In flighthook's Settings > Software, open the **+** and pick
   **Pureplay Accessory**. The card's address is `127.0.0.1:9770`, Pureplay
   on this computer. For Pureplay on another computer, enter the address
   Pureplay's Accessories tab shows, for example `192.168.1.20:9770` (an
   IPv4 address; `localhost` is not accepted).
3. The first time, Pureplay asks the player to approve **flighthook**. The
   card says **Approve flighthook in Pureplay** with the seconds left.
   Approve it with one of Pureplay's two choices (below). After that the
   card connects without asking.
4. The card turns green (**CONNECTED**) once it has Pureplay's round.

Use one card per Pureplay, and point it at the computer hosting the round:
only the host reports shots, and it reports every player's, so the card
takes only those of players seated at that computer. On a computer that
joined someone else's round there are no shots, and Pureplay's readiness is
approximate.

### Read Only or Aim and Club

Pureplay's prompt offers two choices:

| Choice | What flighthook gets |
|---|---|
| **Read Only** | Pureplay's club, hand, player name, shots and results come in. The header's club follows Pureplay: a different pick in the header is set back to Pureplay's club. The card's `club_out` row says `read only`. |
| **Aim and Club** | The same, and a club picked in flighthook's header goes to Pureplay; `club_out` says `on`. |

Either way flighthook never changes the aim. Pureplay's **Let Apps Change
Aim and Club** switch in Settings > Accessories turns club changes off for
every app later on: `club_out` then says `off in Pureplay`, and a header club pick is set
back to Pureplay's until the switch is on again.

### Changing the choice

The choice stays with the pairing. To change it, remove flighthook in
Pureplay's Settings > Accessories, then approve it again with the other
choice. The card deletes its pairing itself when Pureplay says it was
removed; **Forget pairing** is not needed.

- If the card is connected when you remove it, Pureplay tells it at once,
  and the card shows **Removed in Pureplay. Press Reconnect to pair
  again.** Press **Reconnect**.
- If the card only finds out at its next connection (Pureplay or flighthook
  was restarted in between), it asks again by itself, once per start: when
  it has already asked Pureplay to approve it since it last started, it
  shows **Removed in Pureplay. Press Reconnect to pair again.** instead.
- After a removal Pureplay asks apps on that computer to wait about two
  minutes before it shows a new prompt. The card shows **Pureplay asks to
  wait (1:42)** and counts down; when the time is up it asks again by
  itself, Pureplay shows its prompt, and the card pairs as soon as you
  approve. `club_out` and the header's club follow the new choice at once,
  without restarting flighthook.

## The round, golfers and scores (beta)

While connected, the card puts Pureplay's round on the bus as a game: the
players (each as a golfer with a name, hand and bag), the course and the
hole in play, the player up and the scores. Each stroke Pureplay counts is
kept on its shot as a stroke with its outcome and lie. When the round ends
or Pureplay goes away, the round and its players are removed again.

Pureplay's players show up as golfers named by Pureplay. To give one a saved
golfer's name, bag, gender and height, link its Pureplay id to that golfer in
Settings > Golfers: show the golfer, pick the player in the picker
under Linked Profiles (it lists the Pureplay players published right now; the id is the
player's name, `pureplay.<name>`), and Save;
Remove next to a linked id takes it off again. The
shots of that Pureplay player then belong to the saved golfer. A Pureplay
player who is not linked is a guest: their shots keep the name Pureplay gave
them and are listed under it in the Golfer card while they have shots. The
player who is up becomes the golfer up in flighthook, so Sets, the header and
the export follow Pureplay's turn.

Outside a match (range or practice) Pureplay brings no players, so those
shots have no game golfer.

## The card

Connected, the card shows (each row only while it has a value):

| Row | What it is |
|---|---|
| `name` | the player who is up, when that player sits at the Pureplay computer |
| `club` | that player's club in Pureplay |
| `handed` | that player's hand (`rh` or `lh`) |
| `hole` | the hole in play, `7, par 4` |
| `ready` | whether Pureplay waits for the shot (`true` or `false`) |
| `blockers` | while it does not: why (`shot in progress`) |
| `club_out` | whether a club picked in flighthook goes to Pureplay: `on` (Aim and Club), `read only` (Read Only), `off in Pureplay` (Pureplay's **Let Apps Change Aim and Club** is off), `not supported` (this Pureplay has no club command, or the player who is up has no bag) |
| `shots` | the Shots setting; on Auto, where the last shot came from (`auto: last from Pureplay`, `auto: last from flighthook`) |
| `last_shot` | the shot the card took last and when, `#12 at 16:41:07` (stays after the connection) |
| `missed_events` | how many times the card missed some of Pureplay's messages; each time it reads Pureplay's state again |
| `pureplay` | Pureplay's version |

When another player at the same computer is up, the card takes their club,
hand and name. A player seated at another computer changes nothing in
flighthook. A club change made in Pureplay, or by another app such as a
button box, reaches flighthook too.

While the card is connected and has a club for the player who is up, the
header's club follows Pureplay. The card holds Pureplay's club, hand and mode
as one complete pick, and puts it back when anything else changes it. With
`club_out` `on` a header pick goes to Pureplay and then stays; otherwise the
pick is set back to Pureplay's club at once: pick the club in Pureplay. When
Pureplay has no club for a player at this computer (on its menus, or while a
player at another computer is up), the header's club is not set back. The
hand follows Pureplay's profile the same way, and stays as picked for a
profile whose hand Pureplay does not know.

If an output sends shots to a second simulator at the same time (GSPro
alongside Pureplay), that simulator can change the club too, and the last of
the two to change it wins. Use one output, pointed at the Pureplay the card
follows.

## Club picks

With `club_out` `on`, a club picked in flighthook's header (or through the
API) goes to Pureplay for the player who is up:

- Pureplay takes the first club of that type in the player's bag. If the
  player already holds a club of that type nothing is sent, so a second sand
  wedge picked in Pureplay stays.
- A club the player's bag does not hold (flighthook's MD and 1H have no
  Pureplay type) is not sent, and the header goes back to Pureplay's club.
- A pick made while Pureplay can't take it (the ball in the air, the next
  shot not set up yet) waits and goes as soon as Pureplay can, ahead of the
  club Pureplay suggests for that shot. While it waits, the header shows
  the pick and the card's `club` row shows Pureplay's club.
- A waiting pick is dropped when the turn passes to another player, when
  the player picks a club in Pureplay, when the round resets or ends, or
  when the connection drops; the header then shows Pureplay's club. A new
  hole for the same player (a round with one player) keeps it.
- Where Pureplay does not take club changes at all (some drills and modes),
  a pick is dropped at once and the header goes back to Pureplay's club.
- A pick made while a player at another computer is up is not sent. When a
  player at this computer is up again, the header shows their club in
  Pureplay.
- The latest pick wins: picks made while one is on its way replace each
  other.
- When Pureplay refuses a pick, or still says it is not ready after a few
  tries, the header goes back to Pureplay's club.

Clubs from an output (Pureplay's own club coming back through Open Connect)
are never sent back to Pureplay. When Pureplay offers
no direct club pick, only next and previous, the card steps through the bag
until it reaches the type, and gives up (the header goes back to Pureplay's
club) when its steps stop moving.

## Shots

The card's **Shots** setting:

| Shots | What the card takes |
|---|---|
| Auto (default) | every shot of a player at the Pureplay computer that is not one an output sent to a simulator |
| From Pureplay | every shot of a player at the Pureplay computer |
| From flighthook | none; Pureplay's results still reach the shots an output sent |

A shot's values are the ones the game flew the ball with: after Pureplay's
lie penalty or power adjustment, not the launch monitor's own reading. Its
speed, launch angle, direction and spin are all a shot has; the carry,
total and apex the Shots tab shows are flighthook's simulation of those
values, as for every launch monitor. The card's shots are virtual: a
tournament round never takes them, and they are never switched off during
one. A shot only counts from Pureplay's live messages: nothing from before
the card connected, or missed while it was away, is taken later.

### Pureplay's result

Seconds after a shot, Pureplay reports where it ended. flighthook keeps it
on the shot: where the ball ended, penalty strokes, whether it was a putt,
Pureplay's own carry, total and apex, the distances to the hole before and
after, the lie, the hole and the stroke. The JSONL export carries it in the
shot's data (CSV does not); the Shots tab does not show it yet. A shot
Pureplay ends without playing (a gimme, an automatic putt, a penalty
stroke) has no shot in flighthook and no result. A shot the game drops (a
scramble shot the team did not pick) stays in flighthook without a result,
whether the drop comes before or after the result. After a dropped
connection to the same Pureplay the result still finds its shot; after
Pureplay restarts, results for earlier shots are not kept. Pressing the
card's Reconnect, changing its address or turning it off and on starts it
afresh: results for earlier shots are not kept and its shot count starts
at #1 again.

### Camera timing (beta)

The card reports a shot once Pureplay has it from its launch monitor, so
the clip needs the time from the swing to the report: the launch monitor's
own delay and Pureplay's. **Camera timing** in the card's settings
sets it; the default is 0.9 seconds. A card that never set it uses the
default; a value you set stays.

## Pairing

Not connected, the card's `pairing` row says where pairing stands:

| `pairing` | What to do |
|---|---|
| Waiting for Pureplay | Pureplay is showing another app's prompt first; wait |
| Approve flighthook in Pureplay (42 s) | approve it in Pureplay before the time runs out |
| Another app on this computer is waiting for approval in Pureplay | answer that app's prompt; the card asks again by itself |
| Pureplay has too many apps connected | close another app; the card tries again by itself |
| Turn on Ask about new apps in Pureplay's Settings > Accessories | turn it on, then press **Reconnect** |
| Pureplay turned the request down | press **Reconnect** to be asked again |
| The request timed out in Pureplay | press **Reconnect** to be asked again |
| Pureplay asks to wait (1:42) | Pureplay waits after a request was turned down, timed out or removed; the card counts down and asks again by itself when the time is up |
| Removed in Pureplay. Press Reconnect to pair again. | flighthook was removed in Pureplay's Settings > Accessories; press **Reconnect**: after Pureplay's wait the card asks again (see [Changing the choice](#changing-the-choice)) |
| Press Reconnect to pair again. | Pureplay asked to wait twice; press **Reconnect** |

If Pureplay answers that another app is waiting six times in a row, the
card stops and shows Pureplay's own message; press **Reconnect** to try
again.

The card asks Pureplay by itself at most once each time it starts; after
that only **Reconnect** asks again. When Pureplay is closed, or its
Accessories settings change, the card reconnects by itself (after 1, 2, 4
... up to 30 seconds) and needs no new approval. When Pureplay on another
computer goes quiet (the computer sleeps or loses its network), the card
notices within about a minute and reconnects.

An `error` row says the program at the card's address is not a Pureplay
Accessory server, speaks another version of it, or offers none of the
events the card follows (the card tries again every 30 seconds), or that the
pairing could not be saved (the card goes on; flighthook asks again on its
next start).

### Where the pairing is kept

Pureplay gives flighthook a token when the player approves it. flighthook
keeps it in `pureplay-tokens.json` in its data folder
(`%LOCALAPPDATA%\flighthook` on Windows, `~/.local/share/flighthook` on
Linux), readable only by you, and never in the config file: config backups,
exports and screenshots never carry it. The log and the Log tab show
Pureplay's messages with the token replaced by `<redacted>`. If the file is
damaged, flighthook keeps a copy as `pureplay-tokens.json.bad` before it
writes a new one.

The token belongs to the card's address: changing the address pairs again.

**Forget pairing** (the unplug icon) on the card's page deletes the token and
reconnects: Pureplay asks to approve flighthook again. Pureplay keeps the
old entry in its Settings > Accessories until you remove it there.

## Config

```toml
[pureplay_accessory.0]
name = "Pureplay Accessory"
address = "127.0.0.1:9770"
shots = "from_pureplay"
```

`address` is optional (default `127.0.0.1:9770`). `shots` is `auto` (the
default, left out of the file), `from_pureplay` or `from_flighthook`.
`impact_lead_s` is the Camera timing, 0 to 5 seconds (default 0.9, left out
of the file while unset). A card named "Pureplay" loads as it is: the name
is only a label. Shots and Camera timing apply without restarting the card.
The card's API is in [API.md](../API.md);
`POST /api/actors/{id}/forget-pairing` is Forget pairing.
