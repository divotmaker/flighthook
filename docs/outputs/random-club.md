# Random Club

A test aid that stands in for a simulator: it picks a random club and hand
when it starts and again after each finished shot, and makes them
flighthook's, as a simulator that names the club would. Use it to try club
and hand changes without running a simulator. It sends no shots anywhere.

The Software (+) menu does not offer it. Add a `[random_club.N]` section to
the config file.

## Settings

| Field | What it does |
| --- | --- |
| **Name** | The card's name in the UI and the logs. |

```toml
[random_club.0]
name = "Random Club"
```
