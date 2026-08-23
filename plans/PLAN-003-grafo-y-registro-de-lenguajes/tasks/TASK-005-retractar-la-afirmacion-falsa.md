# TASK-005 — Retractar "la cobertura es binaria por símbolo" en docs, protocolos y agentes

- **Plan:** PLAN-003 — Grafo y registro de lenguajes
- **Especialista:** —
- **Proyecto:** DevCtxEngine + configuración global + `~/revfa/.claude/agents/`
- **Depende de:** TASK-001
- **Estado:** `done`

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

- [x] **Paso 1 — Redactar el reemplazo una sola vez**, en EN y en ES, y usarlo en
      todos lados. Debe decir tres cosas: (a) el grafo indexa por nombre
      calificado; (b) desde TASK-001 un nombre pelado se expande a sus formas
      calificadas y **la salida reporta a cuántas declaraciones expandió**;
      (c) **el vacío sigue sin ser prueba de ausencia**, pero ahora por razones
      distintas y verdaderas: despacho dinámico, lenguajes fuera de los 7 con
      gramática, e índice desactualizado.
- [x] **Paso 2 — Actualizar la medición citada** por la de arriba. La vieja
      afirmaba un misterio; la nueva explica un mecanismo.
- [x] **Paso 3 — Docs EN**, los 5.
- [x] **Paso 4 — Docs ES**, los 5. Verificar que cada par EN/ES diga lo mismo.
- [x] **Paso 5 — `~/.claude/CLAUDE.md` y `~/.claude/protocols/devctx-memory.md`.**
      El §4 de CLAUDE.md dice "el grafo es BINARIO por símbolo"; se reemplaza por
      la guía operativa corta: *buscá por nombre pelado, mirá cuántas
      declaraciones reporta, y si te importa una sola, calificala*.
- [x] **Paso 6 — Los 10 agentes REVFA.** Mismo bloque, mismo texto. Es
      sustitución mecánica: **un solo párrafo canónico, no 10 redacciones**.
- [x] **Paso 7 — Commit** (dos: uno en DevCtxEngine, otro donde vivan los agentes).
      `docs: retract the "binary coverage" claim — the cause was the lookup key`

## Criterios de aceptación

- [x] `grep -rn "binary per symbol\|binaria por símbolo\|BINARIO por"` no devuelve
      nada en los 22 archivos.
- [x] Ningún archivo sigue citando `actualizar`/`cambiarEstado` como evidencia de
      cobertura binaria.
- [x] Cada doc EN tiene su par ES diciendo lo mismo.
- [x] Los 10 agentes tienen **texto idéntico** en ese bloque.
- [x] El aviso de que un reporte vacío no prueba ausencia **sigue estando** — con
      las razones correctas. Este no es un cambio de "el grafo ahora es confiable".

## Riesgos

**Sobrecorregir.** La conclusión práctica vieja —"un reporte limpio no prueba
nada, cruzá con `search --keyword`"— **sigue siendo buena**. Lo que estaba mal
era la explicación, no el consejo. Si esta task termina diciendo "ya podés
confiar en el vacío", está mal hecha.

**Los agentes REVFA viven en otro repo.** No mezclar los commits.

## Resultado

- **Estado final:** `done` (2026-08-23)

- **Resumen:** los 22 archivos dejaron de afirmar que la cobertura del grafo es
  binaria por símbolo. En su lugar dicen lo que realmente pasa —el grafo se
  indexa por nombre calificado, un nombre pelado se expande y el reporte dice
  cuántas declaraciones fundió— y **conservan** el aviso de que un vacío no
  prueba ausencia, con las razones correctas.

- **Archivos tocados (22):**
  - **DevCtxEngine, 8 docs** (4 pares EN/ES): `03-core-concepts/symbol-graph.md`,
    `04-agent-workflow.md`, `05-examples/refactoring.md`,
    `05-examples/debugging.md` y sus pares en `docs/es/`.
  - **Config global, 2:** `~/.claude/CLAUDE.md` §4 y
    `~/.claude/protocols/devctx-memory.md`.
  - **Agentes REVFA, 10:** todos los de `~/revfa/.claude/agents/` que llevaban
    el bloque.

- **Verificado por:**
  - Barrido `grep -rn "BINARIO POR SÍMBOLO|binary per symbol|binaria por símbolo"`
    sobre los 22: la única coincidencia que queda es la **nota de retractación**
    que cita la afirmación vieja a propósito, en `symbol-graph.md:126` y su par ES.
  - `grep -c` del bloque nuevo en los agentes → **10 de 10**, y un `sort -u`
    confirma **una sola redacción**, no diez.
  - Cada doc EN tiene su par ES escrito como espejo del mismo texto.

- **Desviaciones:**
  1. **Un solo commit, no dos.** El Paso 7 asumía commits separados, pero ni
     `~/revfa` ni `~/.claude` son repositorios git — verificado con
     `git rev-parse`. Los 12 archivos de configuración y agentes **quedan fuera
     de todo control de versiones**; solo se commitearon los 8 docs. Vale la
     pena decidir aparte si esa configuración debería versionarse.
  2. **Dos variantes de texto, no una.** Los agentes traían el bloque en dos
     formas distintas (9 con un párrafo numerado, `devctx-specialist` con un
     bullet largo de otro formato). Se respetó cada formato con el mismo
     contenido, en vez de uniformar la estructura de los archivos.
  3. **Se agregaron dos razones que faltaban** en la lista de por qué un vacío
     no prueba ausencia: las llamadas hechas fuera de toda función (no hay
     símbolo origen, la arista se descarta) y el índice desactualizado, que se
     ve idéntico a código ausente.

- **Riesgos abiertos / siguiente:**
  - **Los docs describen la expansión, que hoy vive solo en
    `feature/grafo-y-registro-de-lenguajes`.** Viajan con el código, así que
    aterrizan juntos — pero si los docs se publican antes del release, alguien
    en `v0.4.1` va a leer sobre un comportamiento que no tiene.
  - Los 12 archivos fuera de git no tienen respaldo ni historia.
