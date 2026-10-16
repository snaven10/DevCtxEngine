package p;

public class Outer {
    static class Testable {
        int invoke() {
            return Svc.calc();
        }

        int again() {
            return this.invoke();
        }

        static class Deeper {
            int deep() {
                return Svc.calc();
            }
        }
    }

    void run() {
        Testable t = new Testable();
        t.invoke();
    }

    void runDeep() {
        Testable.Deeper d = new Testable.Deeper();
        d.deep();
    }
}
