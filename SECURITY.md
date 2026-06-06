# Security

Kapto runs next to robots and on their networks, so a vulnerability in it
can matter. Please report one privately rather than in a public issue.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting: the **Report a vulnerability**
button in this repository's **Security** tab. Say what is affected (the
command, the endpoint or the setting), how to reproduce it, and what an
attacker could do with it.

You will hear back within a week. Once a fix is ready, it is released and
the report published with credit to you, unless you would rather not be
named.

## Supported versions

Kapto is in early development: fixes go into the latest code on `main`,
and into the latest release once there are releases.

## What Kapto promises

- It makes no outbound connections but to the robot and to the upload
  destinations you configure, and sends no telemetry.
- Its API and web UI listen on loopback by default, and refuse requests
  whose `Host` header doesn't match, against DNS rebinding.
- Credentials are referenced in the configuration (an environment variable
  or a file), never stored in it, in recordings or in logs.

A way around any of these is a vulnerability.
