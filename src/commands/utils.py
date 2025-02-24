from typing import Optional


def confirm_action(prompt: str) -> bool:
    """Prompt user for confirmation"""
    response = input(f"{prompt} [y/N] ").lower()
    return response == "y"


def format_output(data: dict, format: str = "plain") -> str:
    """Format output based on selected format"""
    if format == "json":
        import json

        return json.dumps(data, indent=2)
    elif format == "table":
        from tabulate import tabulate

        return tabulate(data.items(), tablefmt="pretty")
    return "\n".join(f"{k}: {v}" for k, v in data.items())
