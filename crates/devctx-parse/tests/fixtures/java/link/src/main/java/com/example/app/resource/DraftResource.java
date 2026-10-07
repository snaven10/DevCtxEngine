package com.example.app.resource;

import com.example.app.entity.Office;
import com.example.app.service.AlphaService;
import com.example.app.service.BetaService;
import com.example.app.service.GammaService;
import io.smallrye.mutiny.Uni;
import jakarta.persistence.EntityManager;
import java.util.List;
import org.jboss.logging.Logger;

/** Three adapters, each with a `service` field of its own type (PLAN-009 TASK-005). */
public class DraftResource {
    private static final Logger LOG = Logger.getLogger(DraftResource.class);
    EntityManager em;
    private final AuditHelper helper;

    public DraftResource(AuditHelper helper) {
        this.helper = helper;
    }

    static class AlphaServiceAdapter {
        AlphaService service;

        List<String> page(int n) {
            return service.findPaginated(n);
        }
    }

    static class BetaServiceAdapter {
        BetaService service;

        List<String> page(int n) {
            return service.findPaginated(n);
        }
    }

    static class GammaServiceAdapter {
        GammaService service;

        List<String> page(int n) {
            return service.findPaginated(n);
        }
    }

    Object adapter() {
        return new AlphaServiceAdapter();
    }

    void query(String code) {
        var q = em.createQuery("select o from Office o");
        q.setParameter("code", code);
        var created = new Office();
        created.rename(code);
    }

    void log() {
        LOG.info("hello");
    }

    void audit() {
        helper.record("x");
    }

    void imported() {
        AuditUtil.toJson("x");
        Office.normalize("x");
    }

    Uni<Office> find(String c) {
        return Office.findByCodigo(c).flatMap(o -> o.persist());
    }

    void listing() {
        Office.listAll().map(x -> x);
    }

    Object byId(Long id) {
        return Office.findById(id);
    }

    void loop(List<Office> offices) {
        for (Office o : offices) o.rename("a");
        for (var o : offices) o.rename("b");
    }
}
