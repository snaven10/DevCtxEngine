class C:
    def m(self):
        def f():
            leaf()
        f()


def outer():
    def inner():
        leaf()
    inner()


def leaf():
    pass
