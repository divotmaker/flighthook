# Pureplay

flighthook sends shots to [Pureplay](https://github.com/PureplayGolf/Pureplay-Connect)
over GSPro Open Connect. Pureplay accepts Open Connect on TCP port 921, the
same port as GSPro.

## Setup

1. Start Pureplay.
2. In flighthook's Devices tab, open the **+** after Outputs and pick
   **Pureplay (Open Connect)**. It adds the same card as GSPro (Open
   Connect): an Open Connect card at `127.0.0.1:921`, which sends to
   whichever simulator listens on that port. If that card exists, the entry
   adds a second one that starts off (only one card per address can be on);
   the card you have already reaches Pureplay.
3. The card turns green (**CONNECTED**) once Pureplay accepts the
   connection.

Shots, club and hand work as with GSPro; see the Open Connect client in the
[README](../../README.md#simulation-software).
