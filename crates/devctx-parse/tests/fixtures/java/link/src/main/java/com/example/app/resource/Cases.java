package com.example.app.resource;

import com.example.app.entity.Office;
import com.example.app.service.AlphaService;
import java.util.List;
import java.util.Optional;
import lombok.Data;

/** Receivers the review of TASK-005 found mistyped. */
public class Cases {
    @Data
    static class Holder {
        private Office office;
    }

    static class Config {
        public Server server;
    }

    static class Server {
        public int port() {
            return 0;
        }
    }

    Office session;
    Config cfg;
    AlphaService alpha;

    void members(Office target) {
        target.parent.rename("x");
        this.cfg.server.port();
        java.util.List.of();
    }

    void afterExternal(List<Holder> holders, Optional<Office> maybe) {
        holders.get(0).getOffice();
        maybe.get().rename("y");
        maybe.map(o -> o).orElseThrow();
    }

    void shadowed(Object any) {
        alpha.findPaginated(1).forEach(session -> session.rename("z"));
        if (any instanceof Office found) {
            found.rename("w");
        }
    }

    void escalation(Holder holder, Office target) {
        holder.getOffice().rename("v");
        target.rename();
    }
}
