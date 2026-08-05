//! HTTP route (`routes`) table operations: upsert, search, reverse lookup.

use duckdb::params;

use crate::error::Result;
use crate::store::Store;

/// A stored HTTP route.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoredRoute {
    /// Framework (fastapi/flask/express/nestjs/spring/quarkus/angular).
    pub framework: String,
    /// HTTP method (uppercased), or empty.
    pub http_method: String,
    /// Route path.
    pub path: String,
    /// Handler class, if known.
    pub handler_class: String,
    /// Handler method/function, if known.
    pub handler_method: String,
    /// `Class.method` or `method`, for reverse lookup.
    pub handler_symbol: String,
    /// Source file.
    pub file: String,
    /// 1-based line.
    pub line: i32,
}

const ROUTE_COLS: &str = "framework, http_method, path, handler_class, handler_method, \
    handler_symbol, file, line, repo, branch, indexed_at";

impl Store {
    /// Replace all routes originating in `file` with `routes`.
    pub fn replace_file_routes(
        &self,
        repo: &str,
        branch: &str,
        file: &str,
        routes: &[StoredRoute],
        now: &str,
    ) -> Result<()> {
        self.conn.execute(
            "DELETE FROM routes WHERE repo = ? AND branch = ? AND file = ?",
            params![repo, branch, file],
        )?;
        for r in routes {
            self.conn.execute(
                &format!(
                    "INSERT INTO routes ({ROUTE_COLS}) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT DO NOTHING"
                ),
                params![
                    r.framework,
                    r.http_method,
                    r.path,
                    r.handler_class,
                    r.handler_method,
                    r.handler_symbol,
                    r.file,
                    r.line,
                    repo,
                    branch,
                    now,
                ],
            )?;
        }
        Ok(())
    }

    /// Delete all routes originating in a file.
    pub fn delete_file_routes(&self, repo: &str, branch: &str, file: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM routes WHERE repo = ? AND branch = ? AND file = ?",
            params![repo, branch, file],
        )?;
        Ok(())
    }

    /// Search routes by optional HTTP method and path substring.
    pub fn search_routes(
        &self,
        repo: &str,
        branch: &str,
        method: Option<&str>,
        path_like: Option<&str>,
    ) -> Result<Vec<StoredRoute>> {
        let mut clauses = vec!["repo = ?".to_string(), "branch = ?".to_string()];
        let mut binds: Vec<String> = vec![repo.to_string(), branch.to_string()];
        if let Some(m) = method {
            clauses.push("upper(http_method) = ?".to_string());
            binds.push(m.to_uppercase());
        }
        if let Some(p) = path_like {
            clauses.push("path LIKE ?".to_string());
            binds.push(format!("%{p}%"));
        }
        let sql = format!(
            "SELECT {ROUTE_COLS} FROM routes WHERE {} ORDER BY path, http_method",
            clauses.join(" AND ")
        );
        self.query_routes(&sql, binds)
    }

    /// Find routes served by a handler symbol.
    pub fn routes_for_handler(
        &self,
        repo: &str,
        branch: &str,
        handler: &str,
    ) -> Result<Vec<StoredRoute>> {
        let sql = format!(
            "SELECT {ROUTE_COLS} FROM routes
             WHERE repo = ? AND branch = ? AND (handler_symbol = ? OR handler_method = ?)
             ORDER BY path"
        );
        self.query_routes(
            &sql,
            vec![
                repo.to_string(),
                branch.to_string(),
                handler.to_string(),
                handler.to_string(),
            ],
        )
    }

    fn query_routes(&self, sql: &str, binds: Vec<String>) -> Result<Vec<StoredRoute>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(duckdb::params_from_iter(binds), |r| {
            Ok(StoredRoute {
                framework: r.get(0)?,
                http_method: r.get(1)?,
                path: r.get(2)?,
                handler_class: r.get(3)?,
                handler_method: r.get(4)?,
                handler_symbol: r.get(5)?,
                file: r.get(6)?,
                line: r.get(7)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(fw: &str, method: &str, path: &str, handler: &str, file: &str) -> StoredRoute {
        StoredRoute {
            framework: fw.into(),
            http_method: method.into(),
            path: path.into(),
            handler_method: handler.into(),
            handler_symbol: handler.into(),
            file: file.into(),
            line: 1,
            ..Default::default()
        }
    }

    fn seeded() -> Store {
        let store = Store::open_in_memory(3).unwrap();
        store
            .replace_file_routes(
                "repo",
                "main",
                "api.py",
                &[
                    route("fastapi", "GET", "/users", "list_users", "api.py"),
                    route("fastapi", "POST", "/users", "create_user", "api.py"),
                ],
                "100",
            )
            .unwrap();
        store
    }

    #[test]
    fn search_by_method_and_path() {
        let store = seeded();
        assert_eq!(
            store
                .search_routes("repo", "main", None, None)
                .unwrap()
                .len(),
            2
        );
        let gets = store
            .search_routes("repo", "main", Some("get"), None)
            .unwrap();
        assert_eq!(gets.len(), 1);
        assert_eq!(gets[0].path, "/users");
        let users = store
            .search_routes("repo", "main", None, Some("user"))
            .unwrap();
        assert_eq!(users.len(), 2);
    }

    #[test]
    fn reverse_lookup_by_handler() {
        let store = seeded();
        let r = store
            .routes_for_handler("repo", "main", "create_user")
            .unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].http_method, "POST");
    }

    #[test]
    fn replace_is_idempotent() {
        let store = seeded();
        // Re-run with the same file: still 2 routes, not 4.
        store
            .replace_file_routes(
                "repo",
                "main",
                "api.py",
                &[route("fastapi", "GET", "/users", "list_users", "api.py")],
                "200",
            )
            .unwrap();
        assert_eq!(
            store
                .search_routes("repo", "main", None, None)
                .unwrap()
                .len(),
            1
        );
    }
}
