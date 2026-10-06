# TASK-006 — Resolver TypeScript/JavaScript (imports relativos y alias, DI, arrow-const)

- **Plan:** PLAN-009 — Grafo estático preciso y contexto por grafo
- **Especialista:** general-purpose (Rust)
- **Proyecto:** DevCtxEngine (`/home/you/personal/DevCtxEngine`), rama `feat/plan-009-grafo`
- **Depende de:** TASK-005
- **Estado:** `pending`

---

## Objetivo

Que en un repo Angular/Nx (frontend) las llamadas y usos se resuelvan por imports (relativos,
alias de `tsconfig`, barrels `index.ts`), por inyección de dependencias (propiedades de parámetro del
constructor, `inject(T)`) y desde funciones arrow exportadas, con paquetes npm marcados externos.
Implementa DD-7 (TS/JS).

## Contexto verificado

- `typescript.json`/`tsx.json`: `types` solo `public_field_definition`, `variable_declarator` y
  `required_parameter` con `type_identifier`; no captura `accessibility_modifier` de parámetros del
  constructor (propiedades-parámetro), ni `inject(…)`. `javascript.json` no tiene `types`.
- `function_kinds` TS: `function_declaration`, `method_definition` (sin `arrow_function`): las
  llamadas dentro de un `export const x = () =>` hoy no tienen fuente y se descartan
  (`crates/devctx-parse/src/parser.rs:125-127`). TASK-004 las hace símbolo y fuente.
- Caso real: `frontend/lib-auth/src/lib/providers/token.interceptor.ts:78`
  (`export const tokenInterceptor: HttpInterceptorFn = (…) =>`); hay dos `AuthService`
  (`lib-auth/src/lib/services/auth.service.ts:60`, `lib-auth-data/src/lib/services/auth.service.ts:26`):
  el import decide cuál.
- frontend es un monorepo Nx con micro frontends (shell, landing, registry, templates, admin,
  calidad) y librerías con alias (`CLAUDE.md` del workspace).

## Archivos

- **Crear:** `crates/devctx-parse/src/resolve/typescript.rs` (compartido TS/TSX/JS),
  `crates/devctx-index/src/tsconfig.rs` (lectura best-effort de `tsconfig.base.json`/`tsconfig.json`
  de la raíz: `compilerOptions.baseUrl` y `paths`; JSON con comentarios), fixtures multi-archivo en
  `crates/devctx-parse/tests/fixtures/ts/`.
- **Modificar:** `crates/devctx-parse/languages/{typescript,tsx,javascript}.json` (propiedades-
  parámetro, `inject`, `new_expression`, `extends`/`implements`, imports con nombres/alias,
  `platform_prefixes`/globals), `crates/devctx-index/src/link.rs` (import → archivo/símbolo).

## Pasos

- [ ] **Paso 1 — tests que fallan.** Fixture: `auth.service.ts` ×2 en carpetas distintas;
      `interceptor.ts` con `export const tokenInterceptor = (…) => { const s = inject(AuthService);
      s.token(); }` importando uno de los dos por ruta relativa; un componente con
      `constructor(private readonly auth: AuthService)` que llama `this.auth.login()`; un import
      por alias `@acme/auth` mapeado en `tsconfig.base.json`; un barrel `index.ts` que reexporta;
      `rxjs`/`@angular/core` como externos.
- [ ] **Paso 2 — imports.** Relativos (`.ts`, `.tsx`, `.js`, `/index.*`), alias por `paths`
      (best-effort; si no hay `tsconfig`, solo relativos), reexports de barrels a un nivel; npm →
      externo.
- [ ] **Paso 3 — DI.** Propiedades-parámetro del constructor y `inject(T)` → campo tipado
      (`ctor_inject`).
- [ ] **Paso 4 — arrow-const y funciones de módulo** como fuente (TASK-004) y como destino
      importado (`import { tokenInterceptor } from …`).
- [ ] **Paso 5 — JS** con lo que aplique sin tipos (imports, `this`, arrow-const); receptores
      sin tipo → reglas 7-8 (`medium`/`low`).
- [ ] **Paso 6 — medir** sobre frontend (con OK del usuario, es el repo más caro) o sobre una
      sublibrería copiada al scratchpad: gold edges TS, % sin decidir, externos.

## Criterios de aceptación

- [ ] Tests del paso 1 verdes; la llamada desde el interceptor resuelve al `AuthService` importado,
      no al otro (`high`, `import`/`ctor_inject`).
- [ ] Gold edges TS: precisión ≥ 90 % en `high`, ≥ 75 % global.
- [ ] `read_symbol("tokenInterceptor")` tiene definición (lo verifica TASK-008 sobre la tabla;
      acá, que el símbolo está en `symbols` con su firma).

## Riesgos

- `tsconfig` con `extends` encadenados y comentarios: best-effort, un nivel de `extends`; si falla,
  solo relativos y se dice en el resumen del `index` una vez.
- Barrels profundos (reexport de reexport): un nivel; el resto queda `medium`/`low`.

## Resultado

<!-- SE LLENA AL CERRAR (estado done/skipped). Contrato: PLAN-009 §11 -->
- **Estado final:**
- **Resumen:**
- **Archivos tocados:**
- **Verificado por:**
- **Desviaciones:**
- **Riesgos abiertos / siguiente:**
