# Perfetto Tracing

This note documents the current Vixen trace export and the quickest way to
analyze it with SQL.

## Producing a Trace

Set `VX_TRACE=1` for a build that goes through `vxd`.

```sh
VX_TRACE=1 vx build ...
```

The build writes a Chrome/Perfetto-compatible trace to:

```text
.vx/trace.json
```

That file is overwritten by the next traced build. If you want to keep it,
copy it somewhere else first.

```sh
mkdir -p ~/tmp/vixen-traces
cp .vx/trace.json ~/tmp/vixen-traces/trace-$(date +%Y%m%d-%H%M%S).json
```

## Querying the Trace

Use Perfetto's `trace_processor` first. It understands the trace schema
directly and can report trace-health issues that the UI only hints at.

```sh
trace_processor .vx/trace.json
```

For one-shot queries:

```sh
trace_processor -Q "select count(*) as n from slice;" .vx/trace.json
```

The first query to run is:

```sql
select *
from stats
where severity = 'data_loss';
```

If this returns rows such as `misplaced_end_event`, fix the emitter before
trusting the UI.

## Useful Tables

- `slice`: duration events such as `node.execute` and `exec.runner.run`
- `args`: structured span and instant arguments keyed by `arg_set_id`
- `stats`: parser and trace-health diagnostics

`node.execute` and `node.wait` slices carry these args:

- `args.node_id`
- `args.description`
- `args.job_kind`
- `args.source_id`
- `args.source_label`

Exec bridge spans carry args such as:

- `args.executor`
- `args.mount_count`
- `args.tree_mount_count`
- `args.file_mount_count`
- `args.cross_store`
- `args.store_supports_readthrough`

## High-Signal Queries

Total time by slice name:

```sql
select
  name,
  count(*) as n,
  round(sum(dur) / 1e6, 3) as total_ms,
  round(avg(dur) / 1e6, 3) as avg_ms
from slice
group by name
order by sum(dur) desc;
```

Exec-phase breakdown:

```sql
select
  name,
  count(*) as n,
  round(sum(dur) / 1e6, 3) as total_ms,
  round(avg(dur) / 1e6, 3) as avg_ms
from slice
where name like 'exec.%'
group by name
order by sum(dur) desc;
```

Longest `node.execute` slices with metadata:

```sql
select
  s.id,
  round(s.dur / 1e6, 3) as dur_ms,
  max(case when a.key = 'args.node_id' then a.int_value end) as node_id,
  max(case when a.key = 'args.description' then a.string_value end) as description,
  max(case when a.key = 'args.job_kind' then a.string_value end) as job_kind,
  max(case when a.key = 'args.source_id' then a.string_value end) as source_id,
  max(case when a.key = 'args.source_label' then a.string_value end) as source_label
from slice s
left join args a using (arg_set_id)
where s.name = 'node.execute'
group by s.id, s.dur
order by s.dur desc
limit 20;
```

Mount prep versus runner time:

```sql
select
  round(sum(case when name = 'exec.mounts.prepare' then dur else 0 end) / 1e6, 3) as mounts_prepare_ms,
  round(sum(case when name = 'exec.mounts.mirror' then dur else 0 end) / 1e6, 3) as mounts_mirror_ms,
  round(sum(case when name = 'exec.runner.run' then dur else 0 end) / 1e6, 3) as runner_ms,
  round(sum(case when name = 'exec.observer.bridge' then dur else 0 end) / 1e6, 3) as observer_bridge_ms,
  round(sum(case when name = 'exec.result.hydrate' then dur else 0 end) / 1e6, 3) as hydrate_ms
from slice;
```

Per-node exec slices with child exec phases:

```sql
select
  parent.name as parent_name,
  child.name as child_name,
  count(*) as n,
  round(sum(child.dur) / 1e6, 3) as total_ms
from slice child
join slice parent on child.parent_id = parent.id
where parent.name = 'node.execute'
  and child.name like 'exec.%'
group by parent.name, child.name
order by sum(child.dur) desc;
```

## Exporting for Other Tools

If you want to inspect the trace with SQLite or DuckDB, export it first:

```sh
trace_processor -e trace.sqlite .vx/trace.json
duckdb trace.sqlite
```

Start with `trace_processor` anyway. It is the fastest path to "is this build
slow, or is the trace lying to me?"
