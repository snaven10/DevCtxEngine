from typing import Any, Self, TypeVar, Union

from .repo import Item, Repo

T = TypeVar("T")


def anything(repo: Any):
    repo.save(1)


def either(x: Union[Repo, Item]):
    x.save(2)


def obj(x: object):
    x.save(3)


def generic(x: T):
    x.save(4)


def klass(kind: type[Repo]):
    kind.load(None)


class Builder:
    def fluent(self) -> Self:
        return self

    def done(self):
        return 1

    def go(self):
        self.fluent().done()
