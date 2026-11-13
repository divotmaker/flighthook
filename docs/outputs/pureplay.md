# Pureplay

flighthook sends shots to [Pureplay](https://github.com/PureplayGolf/Pureplay-Connect)
over GSPro Open Connect. Pureplay accepts Open Connect on TCP port 921, the
same port as GSPro.

## Setup

1. Start Pureplay.
2. In flighthook's Settings > Software, open the **+** and pick
   **Pureplay (Open Connect)**. It adds the same card as GSPro (Open
   Connect): an Open Connect card at `127.0.0.1:921`, which sends to
   whichever simulator listens on that port. If that card exists, the entry
   adds a second one that starts off (only one card per address can be on);
   the card you have already reaches Pureplay.
3. The card turns green (**CONNECTED**) once Pureplay accepts the
   connection.

Shots, club and hand work as with GSPro; see the
[Open Connect client](open-connect.md), including routing and the club lists
that set the mode.

To follow the round in Pureplay as well (Pureplay's club and hand in
flighthook, and Pureplay's result for each shot kept on it), add the
[Pureplay Accessory card](../devices/pureplay.md) from the same Software
(+) menu: **Pureplay Accessory**. Pureplay reports each shot this card sends
back to the Pureplay Accessory card, which does not take it again. The
Pureplay Accessory card never feeds an output: its own shots and readiness
never go to Pureplay, GSPro or any other simulator, and an output's launch
monitor pickers do not list it.
