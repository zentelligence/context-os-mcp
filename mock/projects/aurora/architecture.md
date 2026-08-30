---
title: Aurora Architecture
entity: aurora
tags:
  - project/aurora
  - area/architecture
---

# Aurora Architecture

## Event Ingestion Pipeline

Readings land on a message queue before a worker validates and writes each one into the time-series store. The event ingestion pipeline is the only place backpressure is handled, and the event ordering guarantee depends on a single partition key per inverter.

## Storage

Readings are partitioned by day in a Postgres schema shared with the reporting service.

## Related notes

See [[projects/aurora/overview]] and [[projects/aurora/risks]].
