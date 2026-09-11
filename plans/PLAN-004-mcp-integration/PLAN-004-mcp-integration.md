# PLAN-004 — MCP integration hardening

**Fecha:** 2026-09-11
**Fase:** Cerrado
**Proyectos:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `fix/mcp-integration`
**Origen:** Auditoría del servidor MCP (`crates/devctx-mcp`) antes de que un agente externo dependa de él a diario.

## 1. Qué resuelve

Cinco defectos independientes del servidor MCP, agrupados porque todos tocan
`crates/devctx-mcp` y conviene revisarlos juntos:

1. **`remember` pierde contenido cuando la sesión no está bindeada.**
   `backend_for` devuelve `Binding::None` → `invalid_request(unbound_help)` antes
   de que `remember` mire el `scope` pedido — a diferencia de `recall`, que sí
   cae a `do_recall_global`. Un `scope: global` explícito fallaba igual que uno
   implícito, y el contenido se tiraba.
2. **Timeout fijo de 3600s por llamada al daemon.** `RemoteClient::agent()`
   aplicaba una hora a *toda* llamada HTTP enrutada — `search`, `read_file`,
   `recall`, no solo `index` — así que un daemon colgado o una red caída dejaban
   al caller esperando una hora para enterarse.
3. **Resultados vacíos silenciosos.** El precedente es el commit `9590e40`:
   `local_recall` leía `/recall` con `as_array()` cuando el endpoint responde
   `{"memories": [...]}`, y el mismatch se leía como "sin memorias" en vez de
   como error.
4. **Costo de contexto de los esquemas de herramientas.** El párrafo de ~230
   caracteres sobre el parámetro `project` está repetido 7 veces; varias
   descripciones de tool son más largas de lo que su contenido informativo
   justifica.
5. **El protocolo de memoria no se expone por MCP.** Vive en
   `MEMORY-PROTOCOL.md` y hoy solo llega a un agente si alguien lo pega a mano
   en `CLAUDE.md`.

## 2. Tasks

| Task | Qué | Archivos principales | Estado |
|------|-----|----------------------|--------|
| 1 | `remember` sin proyecto bindeado guarda como `global` en el store central en vez de fallar; `scope: local` explícito sigue fallando pero con el contenido en el mensaje de error | `crates/devctx-mcp/src/lib.rs`, `crates/devctx-mcp/src/state.rs`, `crates/devctx-cli/tests/mcp_binding.rs` | `done` |
| 2 | Timeout configurable (`DEVCTX_DAEMON_TIMEOUT_SECS`, default 120s); `index_repo` se queda en un timeout largo fijo (1800s) | `crates/devctx-mcp/src/backend.rs`, `docs/11-configuration.md` | `done` |
| 3 | `memories_of` pasa de "forma inesperada → vacío" a "forma inesperada → error"; test unitario para la forma inesperada; se revisó el test `#[ignore]`d de `projects_cli.rs` | `crates/devctx-cli/src/main.rs`, `crates/devctx-cli/tests/projects_cli.rs` | `done` |
| 4 | El párrafo de `project` se recorta a una oración por sitio y se mueve la explicación completa a `instructions`; `remember`/`recall`/`use_project`/`memories_by_symbol`/`memory_move`/`list_projects` se recortan sin perder el comportamiento esencial | `crates/devctx-mcp/src/lib.rs` | `done` |
| 5 | `MEMORY-PROTOCOL.md` embebido (`include_str!`) y servido como prompt `memory-protocol` y recurso `devctx://memory-protocol` | `crates/devctx-mcp/src/lib.rs`, `crates/devctx-cli/tests/mcp_tools.rs` | `done` |

## 3. Fuera de alcance

- No se corrió `cargo build`/`check`/`test` (regla dura del usuario). Todo el
  código se escribió leyendo el código circundante y las fuentes de `rmcp`
  3.1.2 en `~/.cargo/registry`, sin compilar.
- No se tocó `.mcp.json` ni nada bajo `~/.claude`.
- No se renombró ni eliminó ninguna tool — el contrato de nombres se mantiene.

## 4. Cierre

- **Cerrado:** 2026-09-11 — 5 de 5 tasks, sin compilar (ver reporte del agente
  para antes/después de caracteres, riesgos y lo no verificado).
