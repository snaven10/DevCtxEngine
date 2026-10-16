def leaf():
    return 1


def with_lambda():
    f = lambda: leaf()
    return f()


def run_lambda():
    return with_lambda()


class K:
    def m(self):
        g = lambda: leaf()
        return g()

    def run(self):
        return self.m()
