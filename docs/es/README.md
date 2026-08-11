> 🌐 [English documentation](../../README.md#documentation) · 🇪🇸 Español (estás aquí)

# Documentación de DevAI

## Primeros pasos

- [Introducción](01-introduccion.md) — Qué es DevAI, el problema que resuelve, inicio rápido
- [Arquitectura](02-arquitectura.md) — Diagrama del sistema, componentes, flujo de datos
- [Configuración](11-configuracion.md) — config del proyecto y central, variables de entorno, cliente MCP, modo servidor

## Conceptos fundamentales

- [Búsqueda semántica](03-conceptos-fundamentales/busqueda.md) — Pipeline de indexado, chunking, consultas por rama
- [Grafo de símbolos](03-conceptos-fundamentales/grafo-de-simbolos.md) — Grafos de llamadas/imports por AST
- [Memoria](03-conceptos-fundamentales/memoria.md) — Memorias persistentes, deduplicación, upserts por topic key
- [Constructor de contexto](03-conceptos-fundamentales/constructor-de-contexto.md) — Ensamblado de contexto con presupuesto de tokens
- [Integración MCP](03-conceptos-fundamentales/integracion-mcp.md) — Las herramientas MCP, auto-configuración, arquitectura de handlers

## Usando DevAI

- [Flujo de trabajo del agente](04-flujo-de-trabajo-del-agente.md) — Cómo los agentes usan DevAI, patrones de selección de herramientas

## Ejemplos de extremo a extremo

- [Depuración de un bug](05-ejemplos/depuracion.md) — Encontrar un bug de producción con llamadas MCP
- [Incorporación a un codebase](05-ejemplos/incorporacion.md) — De cero a productivo en una sesión
- [Planificación de un refactor](05-ejemplos/refactorizacion.md) — Análisis de radio de impacto con búsqueda + grafo + memoria

## Para contribuir

- [Extender el sistema](06-extender-el-sistema.md) — Agregar herramientas, providers, lenguajes, backends de almacenamiento
- [Rendimiento](07-rendimiento.md) — Latencia, throughput, dimensionamiento, optimización
- [Decisiones de diseño](08-decisiones-de-diseno.md) — Registros de decisiones de arquitectura con tradeoffs
- [Modelos y tuning](09-modelos-embeddings-y-tuning.md) — Modelos de embeddings comparados, estrategias de summarizer y presupuesto de tokens, config por hardware
- [Benchmark de tokens y costo (MCP)](10-benchmark-tokens-mcp.md) — A/B real: recuperación filtrada MCP vs volcado bruto
- [El store central](12-store-central.md) — Registro de proyectos y memoria global compartida entre repos
- [Mantener el índice al día](13-mantener-el-indice-al-dia.md) — hooks, watch, reindex y el planificador
- [Changelog](../../CHANGELOG.md) — Historial de cambios
