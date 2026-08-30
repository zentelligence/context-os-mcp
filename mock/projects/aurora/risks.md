---
title: Aurora Risk Register
entity: aurora
tags:
  - project/aurora
  - area/risk
---

# Aurora Risk Register

## Single Point of Failure

The ingestion worker is currently a single point of failure: if it stops, no telemetry reaches the dashboard until someone restarts it by hand.

## Vendor Risk

The inverter vendor's reporting API has changed twice this year without notice.

## Mitigation

A second ingestion worker is scheduled for the next milestone.
