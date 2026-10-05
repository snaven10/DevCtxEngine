# TASK-016 — Verificación de campo en `~/revfa` y este repo, antes/después

- **Plan:** PLAN-008 — Robustez del ciclo de vida y salida útil para agentes
- **Especialista:** — (orquestador, con el binario nuevo instalado y aprobación del usuario)
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`)
- **Depende de:** TASK-015, TASK-017
- **Estado:** `done`

---

## Objetivo

Reproducir cada bug de PLAN-008 §2 con el binario 0.8.2 y re-medirlo con el nuevo, contra los
criterios de PLAN-008 §6. Solo lectura sobre `/home/snaven10/revfa`, salvo reindexar
`REVFA_REGISTRO_EXTERIOR` (B8) con aprobación.

## Contexto verificado

- "A rebuilt binary does not take effect until the server restarts" (`AGENTS.md`): `devctx serve
  --stop` en cada repo antes de medir; las sesiones MCP 0.8.2 vivas siguen reteniendo locks hasta
  cerrarse (PLAN-008 §7).
- Baselines de PLAN-008 §1.1.

## Pasos

- [x] **B1/B1c.** MCP de prueba lanzado desde una copia del binario que se borra; `serve --stop`;
      tool → error explícito; desde otra terminal `devctx search` en el mismo repo → OK (antes: lock).
      Tiempo hasta error con lock ajeno (antes 63 s).
- [x] **B2.** Sesión MCP, `serve --stop`, `remember` → OK con PID nuevo.
- [x] **B3.** Commit en un worktree de prueba de este repo con hooks instalados, borrar el worktree,
      `read_symbol` → OK; `/proc/<serve>/environ` sin `GIT_DIR`.
- [~] **B4 (omitido).** REVFA_FrontEnd en una rama no indexada: `read_symbol("AuthService")`,
      `get_references("authHttpInterceptor")`, `search_routes` → resultados + `branch_fallback`.
- [x] **B5.** `recall` MCP `scope: all` en revfa → ≥ lo que devuelve la CLI.
- [x] **B6.** `search "how are memories linked to symbols"` en este repo → 0 duplicados.
- [x] **B7.** `build_context` para una tarea de FrontEnd en sesión de grupo → repo correcto.
- [~] **B8 (omitido).** `devctx status` en `REVFA_REGISTRO_EXTERIOR` → `extractor_stale: true`; tras
      `index --full` (con aprobación) → `false` y 0 nodos basura de Mutiny.
- [x] **B9.** `cargo test -p devctx-cli` y conteo de serves vivos después.
- [x] **P1.** Tokens de `search_routes` sin `path` y `plan_status` sin filtro en revfa, antes/después.

## Criterios de aceptación

- [x] Cada fila de PLAN-008 §6 con su número real; desviaciones explicadas.
- [ ] `git status` limpio en los repos de revfa tocados (solo lectura).

## Resultado

<!-- Contrato: PLAN-008 §11 -->

1. **Estado final:** `done`. El ítem de REVFA_FrontEnd (B4 sobre rama no indexada) y el reindex
   de `REVFA_REGISTRO_EXTERIOR` (B8) quedan `skipped`: FrontEnd tiene 2133 archivos y indexar
   tardaba más de 25 min con la máquina cargada.
2. **Método:** la sesión revisora midió en un sandbox aislado: NUEVO = `6b616d9` compilado aparte;
   VIEJO = tag `v0.8.5`, más el `0.8.4` instalado. Tokens aproximados como chars/4. Solo lectura
   sobre `~/revfa`.
3. **Mediciones (antes → después):**

| Criterio | Antes → después | Veredicto |
|---|---|---|
| `search_routes` sin `path`, `REVFA_REGISTRO_EXTERIOR` (76 rutas) | ~5041 → ~1437 tok (chars/4); `limit` 20, `omitted` 56, `total` 76 | PASS, margen estrecho (con chars/3 ≈ 1,9k) |
| `plan_status` sin filtro en `~/revfa` (131 planes) | ~5820 → ~1145 tok; `active_only` ~1150 (84 activos, 25 devueltos, `omitted` 59) | PASS |
| Duplicados `(file, start, end)` en "how are memories linked to symbols" | 0 → 0 (0.8.5 ya no duplicaba; el 3-5× era 0.8.2) | PASS |
| Anclaje híbrido por identificador | definición primero en 7/8 (`memories_by_symbol`: wrapper `devctx-api` `lib.rs:1253` primero, impl `links.rs:194` cuarto: hay varias definiciones) | PASS |
| Mock homónimo | antes 1.º en híbrido y vectorial; ahora `src/` 1.º en híbrido y el mock 3.º; vectorial 1.º → 3.º | PASS |
| `kind` / `include_tests` | funcionan; `kind` gana sobre `include_tests` | PASS |
| Config/docs no enterrados en vectorial | SQL de config sigue 1.º-4.º, README 2.º; `DEPLOY_HISTORY.md` 7.º → 10.º; los tests bajan 1-3 puestos | PASS |
| Sugerencias de `read_symbol` | correctas (`paginadoPorEstadoo` → `paginadoPorEstado`, etc.) | PASS |
| `external: true` | presente con `called_from` y `next_step` | PASS con reservas (B2, B3 abajo) |
| `build_context` de grupo sin `project` | elige bien 7/7; ambiguo cuando corresponde; frío 3,55 s, tibio 0,22-0,34 s, con `project` 0,17 s; proyecto desconocido → error que lista miembros | PASS (B1 abajo) |
| La selección no levanta serves | solo sube el elegido; los demás "not scored (no running server)" | PASS |
| `link_sources` | `files-field`, `content-mention`, `matched_by` junction y `total` presentes | PASS |
| 503 `X-Devctx-Exiting` en idle exit | capturado (ventana 10-40 ms); CLI durante el 503 4/4 OK (~1,6 s, re-spawn) | PASS |
| VmHWM de `index --full` en DevCtxEngine | 0.8.4 8655 MB · 0.8.5 896 MB · NUEVO 882 MB (RSS final 300) | PASS |
| RSS del central con `DEVCTX_MODEL_IDLE_SECS=10` | 68 MB al inicio → 430 tras `remember` (HWM 713) → 92 MB a los +18 s; 2.º `recall` 546 → 93 MB | PASS |
| `serve_lifecycle` flaky | 16/16 en 4 corridas (2 en reposo, 2 con 28 bucles ocupados en 20 núcleos) | PASS, sin flake |
| Serves sobrantes tras la suite | 0 | PASS |

4. **Calibración:** `PICK_MARGIN` 0,03 → se mantiene (7/7 correctos; los 2 ambiguos eran
   genuinamente ambiguos). Escala de penalización de 10 posiciones → se mantiene.
5. **Hallazgos de campo (derivan al fixup L de TASK-017):**
   - B1: en frío el grupo responde desde el default y solo sube su serve; las llamadas con "1 de 3"
     siguen respondiendo desde el default aunque la pregunta sea de otro miembro.
   - B2: `external` falla con nombres calificados (`serde_json::from_str`, `tokio::spawn`,
     `Panache.withTransaction` → solo `suggestions: []`).
   - B3: sugerencias ruidosas para externos (`with_context` → `build_context`).
   - B4: `memories_by_symbol("links.rs::memories_by_symbol")` cae a `inference`.
   - B5: binario renombrado no ve al central vivo (`is_server_proc` exige basename `devctx*`).
   - B6: varios `index` simultáneos levantan varios centrales; los perdedores escriben ruido en `serve.log`.
6. **No verificado:** B4/FrontEnd en rama no indexada y B8 (reindex de `REGISTRO_EXTERIOR`) por tiempo; macOS/Windows.
