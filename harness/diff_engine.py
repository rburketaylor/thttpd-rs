"""Response comparison engine for thttpd golden master testing.
8-field differential comparison."""

import hashlib

def compare_responses(expected, actual):
    """Compare two HTTP responses across 8 fields.
    Returns list of (field, match, expected, actual) tuples."""
    results = []

    checks = [
        ("status_code", expected["status_code"], actual["status_code"]),
        ("status_text", expected["status_text"], actual["status_text"]),
        ("header_count", len(expected["headers"]), len(actual["headers"])),
        ("header_order", list(expected["headers"].keys()), list(actual["headers"].keys())),
        ("header_values", expected["headers"], actual["headers"]),
        ("body_sha256", expected["body_sha256"], actual["body_sha256"]),
        ("body_length", expected["body_length"], actual["body_length"]),
        ("connection_result", expected["connection_result"], actual["connection_result"]),
    ]

    for field, exp, act in checks:
        results.append({
            "field": field,
            "match": exp == act,
            "expected": exp,
            "actual": act,
        })

    return results

def sha256_bytes(data):
    """Compute SHA-256 hash of bytes."""
    return hashlib.sha256(data).hexdigest()
