def pytest_addoption(parser):
    parser.addoption(
        "--burn-seconds",
        type=float,
        default=10.0,
        help="How long thread-safety tests should burn (default: 10s)",
    )
