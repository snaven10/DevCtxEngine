# TASK-002 — CLI: fallo rápido ante un DB bloqueado, con PID y cómo liberarlo

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`)
- **Depende de:** TASK-001
- **Estado:** `done`

---

## Objetivo

`devctx search` (y todo comando CLI que pase por `remote::ensure`) contra un DB retenido por otro
proceso falla en ≤ 2 s, nombrando el PID dueño y la acción para liberarlo (PLAN-008 B1c). Hoy tarda
63 s.

## Contexto verificado

- Callers CLI de `remote::ensure` en `crates/devctx-cli/src/main.rs`: `:725`, `:954` (TUI), `:1564`
  (MCP, lo cubre TASK-001), `:1682`, `:1859`, `:1953`, `:2020`, `:2056`, `:2068`, `:2313`.
- Tras `ensure → None`, los comandos abren el store local (`open_store`) y DuckDB devuelve
  `Could not set lock on file … Conflicting lock is held in <exe> (PID N)`.
- El texto del error de DuckDB ya trae el PID; falta cortar la espera (TASK-001 paso 3) y
  reformularlo.

## Archivos

- **Modificar:** `crates/devctx-cli/src/main.rs` (callers de `ensure`, apertura local)
- **Modificar:** `crates/devctx-cli/src/remote.rs` (si hace falta exponer la causa)

## Pasos

- [x] **Paso 1.** Migrar los callers CLI al `ensure` con causa de TASK-001. Si `ensure` falla por
      lock, no intentar la apertura local: devolver el error directo.
- [x] **Paso 2.** Mensaje: `the index of <proyecto> is held by PID N (<cmdline corto>, started <hora>).
      If it is a `devctx mcp` from an old session, close that session; otherwise `devctx serve --stop`.`
      El cmdline se lee de `/proc/N/cmdline` cuando existe; si el binario es `(deleted)` decirlo.
- [x] **Paso 3.** `DEVCTX_NO_AUTOSERVE=1` + lock: mismo mensaje, sin espera.

## Criterios de aceptación

- [x] Test de integración (CLI): lock ajeno (serve a mano sin `serve.json`) → `devctx search x`
      termina en < 2 s, código ≠ 0, stderr contiene el PID y `serve --stop`.
- [x] Sin lock, el comportamiento y la salida de `devctx search` no cambian (tests existentes verdes).

## Riesgos

Leer `/proc` es Linux-only: en otras plataformas el mensaje omite el cmdline, nunca falla por eso.

## Resultado

1. **Estado final:** `done`.
2. **Repro antes/después.** Antes: `devctx search x` con el DB retenido por otro proceso tardaba ~63 s (auto-spawn esperando, o carga del modelo antes de abrir el store) y terminaba en el texto crudo de DuckDB. Después: test `the_cli_fails_fast_on_a_foreign_lock_naming_the_pid_and_the_remedy` (serve a mano sin `serve.json`; `search` y `symbol`, con y sin `DEVCTX_NO_AUTOSERVE`): código != 0, stderr con `PID <n>` y `serve --stop`, < 2 s (el test completo corre en 0,8 s).
3. **Causa raíz:** confirmada. Con auto-spawn, `ensure` devolvía `None` y se abría el store local (error crudo). Con `DEVCTX_NO_AUTOSERVE`, además, `search` carga el embedder antes de abrir el store (40 s en debug), así que el lock se descubría tarde: hallazgo nuevo.
4. **Archivos y símbolos.**
   - `remote::ensure_cli(&ProjectConfig) -> Result<Option<Remote>>`: `Busy` o lock en la causa de `serve.log` -> `Err` con mensaje; el resto, `Ok(None)` (fallback local como antes). Con `Disabled` sondea el lock antes de dejar que el caller cargue modelos.
   - `remote::is_lock_error(&str) -> bool`, `remote::lock_message(&ProjectConfig, &str) -> String` (PID desde "(PID N)", cmdline de `/proc/N`, `binary replaced (deleted)`, antigüedad; sin `/proc` cae a "another process").
   - `devctx_store::Store::check_unlocked(&Path) -> Result<()>` (abre y suelta; archivo inexistente no es conflicto).
   - `main.rs`: los 14 callers CLI de `remote::ensure` usan `ensure_cli(..)?`; `open_store` reformula el error de lock con el mismo mensaje. `remote::ensure` queda sin callers CLI (lo usa quien necesite `Option`).
5. **Tests nuevos:** integración `the_cli_fails_fast_on_a_foreign_lock_naming_the_pid_and_the_remedy`; unitarios `remote::tests::the_lock_holder_pid_is_read_from_duckdbs_message`, `ages_are_short_and_human`. Comando: `cargo test -p devctx-store -p devctx-cli --test mcp_binding --test mcp_tools --test plan_status_cli --test search_shapes_cli --bins --lib`: bins 55, `mcp_binding` 28, `mcp_tools` 8 (1 ignored previo), `plan_status_cli` 9, `search_shapes_cli` 2, store 55 (1 ignored previo): 0 fallos. `cargo clippy --workspace --all-targets` sin warnings; `cargo fmt --all` aplicado.
6. **Contrato JSON:** sin cambios. Nuevo mensaje de error CLI: `the index of <proyecto> is held by PID N (<cmdline>, running for <edad>). If it is a `devctx mcp` from an old session, close that session; otherwise `devctx serve --stop`.`
7. **No verificado:** plataformas sin `/proc`; TUI ante `Busy` (ahora falla en vez de `reclaim_db`, sin test); el mensaje cuando el binario del dueño está `(deleted)` (sin test).
8. **Números:** lock ajeno -> error en < 2 s (test completo 0,8 s; antes ~63 s). Desviación: "started <hora>" se dio como antigüedad ("running for 3h"), sin dependencia de formato de fechas.

### Fixup D2 (review)

- Busy (serve.json + proceso vivo que no responde): `HEALTH_PATIENT` baja de 3 s a 1,5 s, así que el veredicto
  cuesta 0,4 + 1,5 = 1,9 s, dentro del objetivo de 2 s (eran 3,4 s). Excepción conocida: un serve que tarda entre
  1,5 y 3 s en contestar `/health` ahora se lee como Busy en vez de Up; la llamada siguiente lo alcanza.
- `ensure_cli` (`cli_outcome`): `Spawn(..)` y `Failed` sin causa de lock (o con la línea cortada a 600 caracteres)
  también hacen `Store::check_unlocked` antes de devolver `Ok(None)`, para que quien llama no cargue el embedder
  antes de que `open_store` reformule. Test: `every_unanswered_start_checks_the_lock_before_the_caller_loads_a_model`.
- "running for": ya no usa el mtime de `/proc/<pid>` (el inode se crea al primer lookup: un MCP de 3 días decía
  "2s"); ahora `procown::age_secs` (start ticks de `/proc/<pid>/stat` + `/proc/uptime` + `sysconf(_SC_CLK_TCK)`;
  `ps -o etime=` fuera de Linux). Test: `the_age_shown_is_the_process_age_not_the_procfs_lookup_time`.
- Test del formato de mensaje de DuckDB sin "(PID " en `the_lock_holder_pid_is_read_from_duckdbs_message`.
- N1: "· started background server" se imprime solo tras un arranque exitoso (antes salía antes del error de PID).
- El remedio del mensaje de lock ya no manda en bucle a `serve --stop`: ese comando ahora falla (exit != 0) y dice
  cómo liberar a mano (ver TASK-001 Fixup D2).
