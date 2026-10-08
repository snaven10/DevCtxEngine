from __future__ import annotations
import os
import logging as log
import yaml
import requests
from . import repo as repo_mod
from .lookup import lookup_id
from shop import Repo
from .missing import ghost
from unknownpkg import thing

logger = log.getLogger(__name__)


def helper():
    return 2


class Service:
    def __init__(self, repo: Repo, name):
        self.repo = repo
        self.cache = Repo()
        self.name = name
        self.items = []

    def run(self, data: dict, cb):
        helper()
        helper()
        self.repo.save(data)
        self.cache.save(data)
        lookup_id(1)
        len(data)
        data.get("k")
        os.path.join("a", "b")
        yaml.safe_load("x")
        requests.get("u")
        log.info("x")
        logger.warning("w")
        self.name.strip()
        cb()
        ghost()
        thing()
        repo_mod.Repo().load().price()
        self.again()

    def again(self):
        return None

    def chained(self):
        item = self.repo.load()
        item.price()

    def shadow(self, repo):
        repo.save(1)
        f = lambda repo: repo.save(2)
        [r.save(3) for r in self.items]
        return f


class Plain:
    def __init__(self):
        self.repo = Repo()

    def go(self):
        self.repo.save(4)


def make() -> Repo:
    return Repo()


x = make()
x.save(5)

if __name__ == "__main__":
    helper()
