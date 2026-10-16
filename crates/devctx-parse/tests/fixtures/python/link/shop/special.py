from .repo import Repo
from .lookup import *


class Special(Repo):
    def save(self, item):
        super().save(item)
        self.load()
        lookup_id(2)


class Failure(Exception):
    def go(self):
        self.with_traceback(None)


def first():
    def inner():
        return 1
    return inner()


def second():
    def inner():
        return 2
    return inner()
