-- HIP-0149 retired Proof of Coverage on IoT. The tables below backed the
-- now-deleted PoC pipeline (beacon/witness ingest, entropy, density/reciprocity
-- tracking, and the PoC reward shares) and are no longer read or written by any
-- code. Drop them. Dropping a table also drops its indexes.
--
-- Kept (still in use): meta, gateway_dc_shares, files_processed.

-- Drop the tables first. Both poc_report and gateway_shares carry a
-- `report_type` column of the `reporttype` enum, so the tables must go before
-- the enum types they depend on.
drop table if exists poc_report;        -- PoC beacon/witness report store (loader + runner + purger)
drop table if exists gateway_shares;    -- PoC beacon/witness reward shares (aggregate_poc_shares)
drop table if exists entropy;           -- entropy store (entropy_loader)
drop table if exists last_beacon;       -- last-beacon freshness tracking
drop table if exists last_witness;      -- last-witness freshness tracking
drop table if exists last_beacon_recip; -- HIP-106 beacon reciprocity tracking

-- Now the enum types are unreferenced and can be dropped.
drop type if exists iotstatus;
drop type if exists reporttype;
