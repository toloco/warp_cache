from fast_cache._decorator import cache
from fast_cache._fast_cache_rs import CacheInfo, SharedCacheInfo
from fast_cache._strategies import Backend, Strategy

__all__ = ["Backend", "cache", "CacheInfo", "SharedCacheInfo", "Strategy"]
