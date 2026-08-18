# Decisiones de diseño

> 🇬🇧 [Read in English](../08-design-decisions.md)

Por qué el sistema tiene la forma que tiene. Cada entrada declara la decisión,
el razonamiento y lo que cuesta — las decisiones sin costo son publicidad.

---

## ADR-01: Un solo binario Rust, sin sidecar

**Decisión.** Parseo, chunking, embeddings, reranking, almacenamiento y el
servidor MCP viven todos en un proceso. El único programa externo que se invoca
es `git`.

**Por qué.** El predecesor partía el trabajo entre dos runtimes unidos por un
puente JSON-RPC sobre stdio. Eso compraba librerías de ML nativas del lenguaje y
costaba un ciclo de vida de proceso que administrar, un límite de serialización
en cada llamada, ~880 MB residentes por cliente porque el sidecar no se podía
compartir, y una clase de falla donde una mitad estaba viva y la otra no.

**Costo.** El ecosistema de ML en Rust es más angosto. Algunos modelos existen
en Python y acá no, y a veces exportar a ONNX es el único camino.

## ADR-02: MCP sobre stdio, sin servicio de red

**Decisión.** El servidor MCP habla JSON-RPC 2.0 por stdin/stdout. Todo lo demás
que escucha — `api`, `web` — se ata a loopback.

**Por qué.** El cliente ya administra el ciclo de vida del proceso hijo, así que
stdio obtiene aislamiento y limpieza gratis. No hay puerto que colisione, ni
credencial que guardar, ni autenticación que hacer mal: el límite de confianza
es el límite del proceso.

**Costo.** Un servidor por cliente. No se soporta uso remoto.

## ADR-03: DuckDB para todo

**Decisión.** Una sola base embebida guarda vectores, índice de texto completo,
grafo de símbolos, rutas y memorias. Sin store de vectores aparte.

**Por qué.** La alternativa — un store de vectores al lado de uno relacional —
significa dos ciclos de vida, dos historias de respaldo, y ninguna forma de
filtrar vectores por un predicado relacional sin traer ambos lados a memoria.
DuckDB hace búsqueda vectorial (VSS/HNSW), BM25 (FTS) y SQL común sobre las
mismas filas, así que una búsqueda semántica filtrada es una sola consulta.

**Costo.** DuckDB permite **un solo escritor por archivo**. Esta restricción da
forma a la ADR-04.

## ADR-04: Un servidor de larga vida posee la base

**Decisión.** `devctx serve` sostiene la conexión; los comandos del CLI y las
sesiones MCP rutean a él en vez de abrir el archivo por su cuenta.

**Por qué.** Consecuencia directa de la ADR-03. Sin un dueño, una corrida de
indexado y una búsqueda pelean por el mismo lock y una de las dos falla.

**Costo.** Un proceso que supervisar. Se lanza bajo demanda y se apaga por
inactividad, y el archivo de handshake `serve.json` hay que escribirlo y
borrarlo con cuidado — un servidor que borra el archivo al salir puede dejar
inalcanzable a uno sano, que es un bug que este proyecto efectivamente publicó y
corrigió.

## ADR-05: El WAL no debe sobrevivir al proceso que lo escribió

**Decisión.** Todo camino que termina el servidor hace checkpoint primero, y
también lo hace el final de una corrida de indexado.

**Por qué.** Este es el filo más peligroso del sistema. DuckDB reproduce el WAL
al abrir, pero **un append reproducido no restaura las entradas de un índice
ART** — la estructura detrás de cada `PRIMARY KEY` y `UNIQUE` del esquema. La
tabla queda entonces con filas de las que el índice nunca se enteró, y el
siguiente `DELETE` que las toque aborta con *"Failed to delete all rows from
index"* y tumba la conexión de forma permanente. Re-indexar no lo arregla,
porque re-indexar empieza borrando.

**Costo.** Un checkpoint al final de cada corrida. `devctx repair` existe para
bases que ya están en ese estado: copia cada tabla aparte, la elimina, la recrea
desde el esquema y vuelve a escribir las filas, de modo que el índice ART se
reconstruye a partir de los datos.

## ADR-06: Tirar los índices derivados durante una carga masiva

**Decisión.** Los índices HNSW y FTS se eliminan antes de una corrida grande de
indexado y se reconstruyen después.

**Por qué.** DuckDB mantiene un índice HNSW en **cada insert**, lo que es
catastrófico durante una carga masiva — medido en 7 archivos/minuto con el
índice presente contra 58 archivos/minuto sin él, en el mismo repositorio. FTS
tiene una versión peor del problema: DuckDB no puede mantener un índice FTS
frente a borrados de filas en la tabla indexada, así que un re-indexado que
borra filas aborta de plano.

**Costo.** Una reconstrucción al final, y una ventana durante la corrida donde
la búsqueda aproximada no está disponible.

## ADR-07: Stores por proyecto; compartir solo lo que no tiene dueño

**Decisión.** Cada repositorio tiene su propia base. Un store central guarda
solo el registro de proyectos y las memorias explícitamente globales o de grupo.

**Por qué.** Un diseño anterior apuntaba todos los repositorios a una sola base.
Los stores por proyecto hacen que re-indexar un repositorio nunca bloquee a
otro, que cada uno pueda usar un modelo de embeddings distinto, y que ninguna
búsqueda necesite un filtro de repositorio para ser correcta.

**Costo.** La búsqueda entre proyectos es una llamada explícita
(`search_project`), no un default.

## ADR-08: Copias por rama, guiadas por hash de contenido

**Decisión.** Los fragmentos se guardan por `(repo, rama)`. Indexar una segunda
rama copia las filas de los archivos cuyo hash de contenido coincide, en vez de
re-embeberlas.

**Por qué.** *Esto revierte un diseño anterior.* El predecesor usaba un overlay
de ramas: un índice base más un diff. Los overlays son elegantes y acá están
mal — cada lectura paga una fusión, y la fusión tiene que saber qué lado gana
para un archivo tocado en ambos. Las copias hacen que una lectura sea una
consulta filtrada común.

Copiar es asequible porque lo caro es el embedding, no el almacenamiento, y las
ramas comparten casi todo su contenido. Medido en tres repositorios reales:
**95–96% de los archivos copiados en vez de re-embebidos**.

**Costo.** El almacenamiento crece de forma aproximadamente lineal con las ramas
declaradas. Y un caveat conocido: cambiar `indexing.exclude` entre corridas no
se refleja en el hash de contenido, así que la deduplicación puede copiar filas
que las nuevas exclusiones habrían descartado.

## ADR-09: Chunking consciente del AST, nunca partir un símbolo

**Decisión.** Los límites de fragmento salen de los parseos de tree-sitter, en
niveles archivo / clase / doc / función / bloque.

**Por qué.** El chunking de ventana fija parte una función entre dos fragmentos,
y ninguna de las mitades se embebe como aquello que la función hace. Los límites
de símbolo son las unidades sobre las que la gente hace preguntas.

**Costo.** Una gramática por lenguaje. Los archivos en lenguajes no soportados
caen a fragmentos de texto crudo con solapamiento.

## ADR-10: Indexado incremental desde el diff de git, contra el árbol de trabajo

**Decisión.** El indexado calcula qué cambió vía git, pero indexa el **árbol de
trabajo**, no el último commit.

**Por qué.** Un archivo que escribiste pero no commiteaste es exactamente el
código sobre el que más probablemente vas a preguntar.

**Costo.** Todo lo que git ignora nunca llega al índice — `.gitignore` es el
primer lugar donde controlar qué se indexa, cosa que sorprende una vez.

## ADR-11: Identidad de memoria por clave de tema, con caída a hash de contenido

**Decisión.** `--topic` hace upsert. Sin él, la identidad es un hash sobre el
contenido normalizado.

**Por qué.** Dos modos de falla distintos necesitan dos respuestas distintas.
Una decisión que se revisa tiene que reemplazarse a sí misma o el store acumula
versiones contradictorias — eso es la clave de tema. Una observación guardada
dos veces por un agente entusiasta no debe volverse dos filas — eso es el hash
de contenido.

**Costo.** Quien escribe la memoria tiene que decidir en cuál de los dos casos
está.

## ADR-12: Las memorias globales y de grupo se re-llavean, no se etiquetan

**Decisión.** Las filas globales llevan el proyecto reservado `@global`; las de
grupo llevan `@group:<nombre>`. El repositorio que las aportó sobrevive en
`repo`.

**Por qué.** La identidad deriva de proyecto + hash de contenido. Si una fila
global conservara su proyecto contribuyente, la misma lección aprendida en dos
repositorios caería como dos filas — la deduplicación fallando justo donde
compartir más importa.

**Costo.** `project` deja de ser una clave foránea común, y el código que la lee
tiene que conocer los valores reservados.

## ADR-13: La fila de unión vive en el store del proyecto

**Decisión.** Un vínculo memoria↔símbolo se escribe en la base del **proyecto** y
lleva solo el id de la memoria, incluso cuando la memoria misma vive en el
central.

**Por qué.** El grafo de llamadas es por repositorio; una memoria global no lo
es. Una memoria sobre el `charge()` de este repositorio tiene que ser
encontrable desde `charge()` sin importar cuál store guarde su texto. Resolver
el id busca primero local, después central.

**Costo.** Una indirección de búsqueda. La alternativa — copiar el texto de la
memoria a cada proyecto que la menciona — dejaría copias rancias detrás de cada
edición.

## ADR-14: La procedencia del vínculo se devuelve, no se esconde

**Decisión.** Todo resultado de memoria-por-código lleva `link_sources`:
`files-field`, `content-mention` o `inference`.

**Por qué.** Los dos primeros significan que algo conectó la memoria con el
código al momento de escribirla. El tercero significa solo que coincidieron
palabras. Colapsarlos en un solo indicador de "relacionado" presentaría una
conjetura con la misma confianza que un hecho.

**Costo.** Quien llame tiene que leer un campo para saber cuánto confiar en un
resultado.

## ADR-15: Reranking apagado por defecto

**Decisión.** El cross-encoder está deshabilitado salvo que se configure
encendido.

**Por qué.** Medición, no principio. En este repositorio una búsqueda cuesta 30
ms y 406 MB residentes; el cross-encoder más barato la lleva a 8.6 s y 2.4 GB, y
`bge-reranker-base` a 30 s y 3.4 GB. Lo que eso compra es reordenar una lista
que el recuperador ya tenía bien — y el único modelo medido contra todo el banco
la empeoró, bajando una respuesta correcta del primer puesto al vigésimo
primero.

**Costo.** El orden es el del recuperador. Todo lo que la etapa de recuperación
encontró se devuelve igual.

## ADR-16: Degradación elegante antes que falla dura

**Decisión.** La búsqueda híbrida cae a solo vectorial cuando falta el índice
FTS. El vinculado es de mejor esfuerzo y devuelve un conteo en vez de un error.

**Por qué.** Son enriquecimientos. Un repositorio todavía no indexado del todo
no debe convertir un `remember` exitoso en una falla, y un índice de palabras
clave ausente debería acotar resultados en vez de romper la consulta.

**Costo.** La degradación silenciosa es un peligro real — acá es aceptable solo
porque el resultado degradado sigue siendo correcto, apenas menos bueno. Donde
truncar sería *engañoso* en vez de meramente peor, el sistema lo dice: ver
`build_context`, que nombra lo que no entró.

## ADR-17: `build_context` devuelve prosa

**Decisión.** Una herramienta devuelve texto en vez de JSON.

**Por qué.** Su salida está pensada para leerse directo al contexto de un
modelo. Un sobre JSON alrededor de código y prosa gasta presupuesto en
puntuación y compra estructura que nadie parsea.

**Costo.** Inconsistente con las otras 22 herramientas, que devuelven JSON.
