try:
    from .repo import Repo as Store
except ImportError:
    from .special import Special as Store


def build():
    Store()


def deco(f):
    return f


@deco
def decorated():
    return 1


def use():
    decorated()
