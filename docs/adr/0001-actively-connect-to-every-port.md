# 1. Actively connect to every port, rather than only reading process state

Date: 2026-09-02

## Status

Accepted

## Context

The dashboard must tell the user which local servers are worth stopping. The obvious
approach is passive: read what the operating system already knows — which programs
hold which ports, how long they have run, how many connections they have — and never
touch the ports themselves. A monitoring tool that only observes is easy to trust.

Passive observation was tried first against a real machine and failed.

Two candidate signals for "this server is finished with" were tested:

- **Age.** A service had been running seven days and was entirely wanted. Meanwhile
  a dead server was three days old, the same age as a healthy one beside it.
- **Established connections.** Every development server observed had zero, including
  the ones the user wanted. A closed browser tab reads identically to an abandoned
  server.

Then the interesting case appeared. One development server had held port 4321 for
three days and **refused every connection**. The process was alive, the port was
bound, and the server was dead. Nothing in the operating system's own bookkeeping
distinguished it from the healthy server running beside it in the same project.

The only thing that revealed it was connecting to it.

This is uncomfortable. It means a passive-looking dashboard reaching out and touching
every port on the user's machine, including databases, mail services, and programs
belonging to applications it has nothing to do with. A future reader will reasonably
ask why a monitoring tool generates traffic.

## Decision

Actively check every server by opening a TCP connection to it on every refresh, and
carrying no protocol data on that connection.

The connection alone answers the question. Because it speaks no protocol, it is safe
against services that are not web servers: a database sees a connection open and
close, not a malformed request to log.

Full protocol requests — a `GET /` to learn a server's title — are restricted to the
user's own development servers, sent once, and cached. They are never sent to
background services, application helpers, or anything belonging to macOS.

## Consequences

The tool detects the failure mode that motivated it. The dead-but-listening server
is the case the user cannot otherwise see, and it is now the most valuable signal the
dashboard has.

The tool generates traffic on the user's machine. This is a real cost, bounded by
design: connection checks carry no data, and protocol requests are confined to the
user's own development servers and cached rather than repeated.

The cost was measured rather than assumed. A full cycle across 30 listeners took
191ms in shell, and a dead port refuses in 10ms rather than hanging to a timeout, so
the pathological case is the cheap one. At a 5-second refresh this is under 1% of one
core.

Had the connection check proved expensive or unsafe, the alternative was to make it
a manual per-row button. The measurements made that unnecessary.
