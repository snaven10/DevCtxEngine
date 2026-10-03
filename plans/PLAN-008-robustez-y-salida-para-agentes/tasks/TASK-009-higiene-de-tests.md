# TASK-009 — Higiene de tests: ningún serve sobrevive a la suite

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** rust (modelo sugerido: sonnet)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** TASK-001
- **Estado:** `pending`

---

## Objetivo

Que después de `cargo test -p devctx-cli` no quede ningún `devctx serve` vivo, y que un serve cuyo
proyecto desapareció se apague solo (PLAN-008 B9).

## Contexto verificado

- `crates/devctx-cli/tests/mcp_binding.rs:93-132` — `Tmp::drop`: `serve --stop` en proyectos a 1 y
  2 niveles + `serve --central --stop` + `remove_dir_all`.
- `mcp_binding.rs:700` — `Tmp::new("localguard-solo")` en `local_scope_refuses_to_guess_a_repository`
  (`:668`); el serve filtrado tenía cwd en ese tmp borrado y `--idle 900`.
- `crates/devctx-cli/src/remote.rs:224-245` — `stop_server` lee `serve.json`; sin archivo no hay a
  quién parar.
- TASK-001 cambia `cmd_serve` para escribir `serve.json` después de abrir el store: amplía la
  ventana en la que un serve vive sin anunciarse.

## Archivos

- **Modificar:** `crates/devctx-cli/tests/mcp_binding.rs` (`Tmp::drop`)
- **Modificar:** `crates/devctx-api/src/lib.rs` (vigilancia del directorio del proyecto)

## Pasos

- [ ] **Paso 1 — Repro.** Correr solo `local_scope_refuses_to_guess_a_repository` y listar
      `devctx serve` vivos con cwd en `/tmp/devctx_bind_it_*` antes/después. Confirmar la causa.
- [ ] **Paso 2 — Producto.** El serve sale (limpio, borrando su `serve.json` si es suyo) cuando el
      directorio del proyecto o su `.devctx/` desaparecen; chequeo barato en el loop de idle
      existente (`devctx-api/src/lib.rs`, ~cada 10-30 s).
- [ ] **Paso 3 — Tests.** `Tmp::drop` además espera a que no quede ningún proceso cuyo cwd esté bajo
      el tmp (Linux: `/proc/*/cwd`), con tope corto; y `Tmp` exporta un `--idle` corto para los serves
      auto-lanzados en tests (variable de entorno leída por `spawn_server`, p. ej.
      `DEVCTX_AUTOSERVE_IDLE`).

## Criterios de aceptación

- [ ] Tras `cargo test -p devctx-cli --test mcp_binding`, 0 procesos `devctx serve` con cwd bajo
      `/tmp/devctx_bind_it_*` (verificado en el Resultado con el comando usado).
- [ ] Test: serve sobre un proyecto en tmp; borrar el directorio → el serve sale en < 60 s.

## Riesgos

Un directorio de proyecto en un disco montado de forma intermitente haría salir al serve; aceptado
(se relanza en la próxima llamada).

## Resultado

<!-- Contrato: PLAN-008 §11 -->
