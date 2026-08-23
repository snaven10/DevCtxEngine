# TASK-005 — Retractar "la cobertura es binaria por símbolo" en docs, protocolos y agentes

- **Plan:** PLAN-003 — Grafo y registro de lenguajes
- **Especialista:** —
- **Proyecto:** DevCtxEngine + configuración global + `~/revfa/.claude/agents/`
- **Depende de:** TASK-001
- **Estado:** `pending`

---

## Objetivo

Que la documentación deje de afirmar algo falso. Hoy dice que la cobertura del
grafo es binaria por símbolo y que **nada distingue** un símbolo completo de uno
vacío. Sí hay algo que los distingue, es determinista, y cabe en una línea.

## Contexto verificado (2026-08-23)

**La afirmación falsa:** *"Coverage is binary per symbol… nothing says in advance
which group your symbol is in."*

**Lo que realmente pasa:** el `source` de una arista está **siempre** calificado
(`Clase.metodo`); el `target` está calificado **solo si** el sitio de llamada
tenía un receptor con tipo resoluble. La consulta era por igualdad exacta. Por eso
`crearNotificacion` (llamado intra-clase, sin `this.`) devolvía sus 8 aristas y
`actualizar` (llamado vía campo tipado) devolvía cero. **Las aristas de
`actualizar` siempre existieron** — bajo la llave `OficinaService.actualizar`.

Medición que hay que reemplazar, no borrar:

```
devctx impact actualizar                 → 0 callers, 0 callees
devctx impact OficinaService.actualizar  → 1 caller, 23 callees
devctx impact crearNotificacion          → 8 callers directos
```

### Inventario completo (grep, 2026-08-23)

**DevCtxEngine — 10 archivos:**

| EN | ES |
|---|---|
| `docs/03-core-concepts/symbol-graph.md:113` | `docs/es/03-conceptos-fundamentales/grafo-de-simbolos.md:117` |
| `docs/04-agent-workflow.md:75` | `docs/es/04-flujo-de-trabajo-del-agente.md` |
| `docs/05-examples/refactoring.md:34,41` | `docs/es/05-ejemplos/refactorizacion.md:35,42` |
| `docs/05-examples/debugging.md:83` | `docs/es/05-ejemplos/depuracion.md:83` |

**Configuración global — 2 archivos:**
`~/.claude/CLAUDE.md:94` · `~/.claude/protocols/devctx-memory.md`

**Agentes REVFA — 10 archivos** (bloque "Code Intelligence"):
`java-backend-specialist`, `devctx-specialist`, `frontend-ui-debugger`,
`code-reviewer`, `external-api-integration-specialist`,
`angular-frontend-architect`, `oracle-database-specialist`,
`authentication-specialist`, `legacy-data-analyst`, `openkm-documents-specialist`
— todos en `~/revfa/.claude/agents/`.

## Archivos

- **Modificar:** los 22 listados arriba.

## Pasos

- [ ] **Paso 1 — Redactar el reemplazo una sola vez**, en EN y en ES, y usarlo en
      todos lados. Debe decir tres cosas: (a) el grafo indexa por nombre
      calificado; (b) desde TASK-001 un nombre pelado se expande a sus formas
      calificadas y **la salida reporta a cuántas declaraciones expandió**;
      (c) **el vacío sigue sin ser prueba de ausencia**, pero ahora por razones
      distintas y verdaderas: despacho dinámico, lenguajes fuera de los 7 con
      gramática, e índice desactualizado.
- [ ] **Paso 2 — Actualizar la medición citada** por la de arriba. La vieja
      afirmaba un misterio; la nueva explica un mecanismo.
- [ ] **Paso 3 — Docs EN**, los 5.
- [ ] **Paso 4 — Docs ES**, los 5. Verificar que cada par EN/ES diga lo mismo.
- [ ] **Paso 5 — `~/.claude/CLAUDE.md` y `~/.claude/protocols/devctx-memory.md`.**
      El §4 de CLAUDE.md dice "el grafo es BINARIO por símbolo"; se reemplaza por
      la guía operativa corta: *buscá por nombre pelado, mirá cuántas
      declaraciones reporta, y si te importa una sola, calificala*.
- [ ] **Paso 6 — Los 10 agentes REVFA.** Mismo bloque, mismo texto. Es
      sustitución mecánica: **un solo párrafo canónico, no 10 redacciones**.
- [ ] **Paso 7 — Commit** (dos: uno en DevCtxEngine, otro donde vivan los agentes).
      `docs: retract the "binary coverage" claim — the cause was the lookup key`

## Criterios de aceptación

- [ ] `grep -rn "binary per symbol\|binaria por símbolo\|BINARIO por"` no devuelve
      nada en los 22 archivos.
- [ ] Ningún archivo sigue citando `actualizar`/`cambiarEstado` como evidencia de
      cobertura binaria.
- [ ] Cada doc EN tiene su par ES diciendo lo mismo.
- [ ] Los 10 agentes tienen **texto idéntico** en ese bloque.
- [ ] El aviso de que un reporte vacío no prueba ausencia **sigue estando** — con
      las razones correctas. Este no es un cambio de "el grafo ahora es confiable".

## Riesgos

**Sobrecorregir.** La conclusión práctica vieja —"un reporte limpio no prueba
nada, cruzá con `search --keyword`"— **sigue siendo buena**. Lo que estaba mal
era la explicación, no el consejo. Si esta task termina diciendo "ya podés
confiar en el vacío", está mal hecha.

**Los agentes REVFA viven en otro repo.** No mezclar los commits.

## Resultado
<!-- SE LLENA AL CERRAR -->
