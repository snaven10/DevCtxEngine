class Repo:
    def save(self, item):
        return item

    def load(self) -> "Item":
        return Item()


class Item:
    def price(self):
        return 1
