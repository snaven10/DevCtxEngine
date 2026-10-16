package p;

import java.util.List;

public class Typed {
    private com.example.store.Store fq;
    private Store plain;

    int byFullName() {
        return fq.load();
    }

    int byPlainName() {
        return plain.load();
    }

    int byLocal() {
        Store local = new Store();
        return local.load();
    }

    int byParam(Store given) {
        return given.load();
    }

    int byThisField() {
        return this.plain.load();
    }

    int chained(List<Store> all) {
        return all.get(0).load();
    }

    static class Inner {
        int viaOuterName(Store plain) {
            return plain.load();
        }
    }
}
