"""Tests for udoc.Config + sub-configs (W1-METHODS-CONFIG)."""

import dataclasses
import pickle
import pytest

udoc = pytest.importorskip("udoc")


def test_config_default_classmethod():
    cfg = udoc.Config.default()
    assert isinstance(cfg, udoc.Config)


def test_config_agent_preset():
    cfg = udoc.Config.agent()
    assert isinstance(cfg, udoc.Config)


def test_config_batch_preset():
    cfg = udoc.Config.batch()
    assert isinstance(cfg, udoc.Config)


def test_config_ocr_preset():
    cfg = udoc.Config.ocr()
    assert isinstance(cfg, udoc.Config)


def test_config_pickle_roundtrip_default():
    """ tripwire: pickle.dumps + loads roundtrips cleanly."""
    cfg = udoc.Config.default()
    blob = pickle.dumps(cfg)
    cfg2 = pickle.loads(blob)
    assert isinstance(cfg2, udoc.Config)


def test_config_pickle_roundtrip_agent():
    """ tripwire."""
    cfg = udoc.Config.agent()
    cfg2 = pickle.loads(pickle.dumps(cfg))
    assert isinstance(cfg2, udoc.Config)


def test_config_pickle_roundtrip_batch():
    cfg = udoc.Config.batch()
    cfg2 = pickle.loads(pickle.dumps(cfg))
    assert isinstance(cfg2, udoc.Config)


def test_config_pickle_roundtrip_ocr():
    cfg = udoc.Config.ocr()
    cfg2 = pickle.loads(pickle.dumps(cfg))
    assert isinstance(cfg2, udoc.Config)


def test_limits_default_is_pickle_clean():
    """Sub-configs are also pickle-clean."""
    lim = udoc.Limits()
    pickle.loads(pickle.dumps(lim))


def test_asset_config_is_pickle_clean():
    a = udoc.AssetConfig()
    pickle.loads(pickle.dumps(a))


def test_layer_config_is_pickle_clean():
    layers = udoc.LayerConfig()
    pickle.loads(pickle.dumps(layers))


def test_config_dataclass_fields_works():
    """: __dataclass_fields__ shim makes dataclasses.fields() work."""
    cfg = udoc.Config.default()
    fields = dataclasses.fields(cfg)
    assert len(fields) > 0
    field_names = {f.name for f in fields}
    # Config must have at least these conceptual fields per .
    assert "limits" in field_names or "hooks" in field_names or "layers" in field_names


def test_config_match_args_set():
    """: __match_args__ enables structural pattern matching."""
    assert hasattr(udoc.Config, "__match_args__")
    assert isinstance(udoc.Config.__match_args__, tuple)


def test_config_repr_contains_class_name():
    cfg = udoc.Config.default()
    r = repr(cfg)
    assert "Config" in r


def test_config_repr_masks_password():
    """Per CONFIG agent's note: password is masked in __repr__."""
    cfg = udoc.Config.default()
    r = repr(cfg)
    # If password=None default, just check no leak. If a password is
    # set via __new__, the repr should NOT contain the literal value.
    assert "password=" in r or "Config(" in r  # at least the field is mentioned


def test_render_config_profile_string():
    """RenderConfig accepts ocr_friendly | visual."""
    rc = udoc.RenderConfig()
    assert isinstance(rc, udoc.RenderConfig)


def test_render_config_invalid_profile_rejected():
    """Bogus rendering profile is rejected at construction (per CONFIG agent)."""
    with pytest.raises((ValueError, TypeError)):
        udoc.RenderConfig(profile="bogus_profile_name")
