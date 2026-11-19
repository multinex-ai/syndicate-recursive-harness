#!/usr/bin/env node
import assert from "node:assert/strict";
import test from "node:test";
import { auditConfigText } from "./audit-ot-exposure.mjs";

test("auditConfigText flags internet-exposed Unitronics, Modbus, S7comm, and VNC ports plus default PIN 1111", () => {
  const vulnerableCompose = `
services:
  water-booster-plc:
    image: unitronics-gateway:latest
    network_mode: "host"
    ports:
      - "0.0.0.0:20256:20256"
      - "502:502"
      - "0.0.0.0:5900:5900"
    environment:
      PLC_PIN: "1111"
      PLC_MODE: "REMOTE_PROG"
      ANONYMOUS_WRITE: true
`;
  const findings = auditConfigText("docker-compose.vulnerable.yml", vulnerableCompose);
  const ids = new Set(findings.map((f) => f.id));
  assert.ok(ids.has("OT-PORT-001"), "should flag exposed OT/VNC ports");
  assert.ok(ids.has("OT-CRED-001"), "should flag Unitronics default PIN 1111");
  assert.ok(ids.has("OT-CRED-003"), "should flag anonymous write access");
  assert.ok(ids.has("OT-SEG-001"), "should flag network_mode: host");
  assert.ok(ids.has("OT-PLC-001"), "should flag REMOTE_PROG mode");
});

test("auditConfigText passes loopback-bound or private-tunnel-bound OT configs with rotated credentials", () => {
  const hardenedCompose = `
services:
  water-booster-plc:
    image: unitronics-gateway:latest
    ports:
      - "127.0.0.1:20256:20256"
      - "100.64.0.15:502:502"
    environment:
      PLC_MODE: "RUN"
`;
  const findings = auditConfigText("docker-compose.hardened.yml", hardenedCompose);
  assert.equal(findings.length, 0);
});
