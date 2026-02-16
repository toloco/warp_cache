from enum import IntEnum


class Strategy(IntEnum):
    LRU = 0
    MRU = 1
    FIFO = 2
    LFU = 3


class Backend(IntEnum):
    MEMORY = 0
    SHARED = 1
