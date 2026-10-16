package p;

import java.util.function.Supplier;

public class Lam {
    int withLambda() {
        Supplier<Integer> s = () -> Svc.calc();
        return s.get();
    }

    int withAnon() {
        Supplier<Integer> s = new Supplier<Integer>() {
            @Override
            public Integer get() {
                return Svc.calc();
            }
        };
        return s.get();
    }

    int runLambda() {
        return withLambda();
    }

    int runAnon() {
        return withAnon();
    }
}
